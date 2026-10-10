import { afterAll, beforeAll, expect, test } from "vitest";
import { Gateway, GatewayError } from "../src/gateway";
import { prepareDeposit, EXPIRY_BLOCKS } from "../src/deposit";
import { hex, sessionFromSeed } from "../src/session";
import { encodeOrchardUa, orchardReceiverFromUa } from "../src/ua";
import { encodeMemoText } from "../src/wire";
import { startMockGateway } from "./mock-gateway";

const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const vault = new Uint8Array(43).fill(0x33);
let mock: Awaited<ReturnType<typeof startMockGateway>>;
let mock2: Awaited<ReturnType<typeof startMockGateway>>;

beforeAll(async () => {
  mock = await startMockGateway({ app: APP, network: "regtest", vaultAddress: vault, tip: 2_000 });
  mock2 = await startMockGateway({ app: APP, network: "regtest", vaultAddress: vault, tip: 2_000 });
});
afterAll(() => {
  mock.server.close();
  mock2.server.close();
});

const payout = encodeOrchardUa(new Uint8Array(43).fill(0x22), "regtest");

test("deposit flow: signed memo registered, session UA, ZIP 321 link", async () => {
  const gw = new Gateway(mock.url);
  const s = sessionFromSeed(new Uint8Array(32).fill(9));
  const d = await prepareDeposit(gw, APP, s, payout, 1_000_000n, true);
  expect(mock.intents.get(hex(s.publicKey))).toBeDefined();
  expect(d.memo.expiryHeight).toBe(2_000 + EXPIRY_BLOCKS);
  expect(d.memo.flags).toBe(1);
  expect(hex(d.memo.payoutAddress)).toBe("22".repeat(43));
  expect(orchardReceiverFromUa(d.session.ua, "regtest")).toHaveLength(43);
  expect(d.link).toContain(`zcash:${d.session.ua}?amount=0.01&memo=`);
  const memoParam = new URL(d.link.replace("zcash:", "zcash://x/")).searchParams.get("memo")!;
  expect(Buffer.from(memoParam, "base64url").toString()).toBe(encodeMemoText(d.memo));
  const info = await gw.session(APP, hex(s.publicKey));
  expect(info.intent_registered).toBe(true);
});

test("the intent goes to every operator; one operator down is reported, not fatal", async () => {
  const s = sessionFromSeed(new Uint8Array(32).fill(12));
  const d = await prepareDeposit([new Gateway(mock.url), new Gateway(mock2.url), new Gateway("http://127.0.0.1:9")], APP, s, payout, 1_000_000n);
  expect(mock.intents.has(hex(s.publicKey)) && mock2.intents.has(hex(s.publicKey))).toBe(true);
  expect(d.registered).toEqual([mock.url, mock2.url]);
  expect(d.failed).toHaveLength(1);
});

test("deposit status polling returns the gateway's states", async () => {
  const gw = new Gateway(mock.url);
  const s = sessionFromSeed(new Uint8Array(32).fill(10));
  expect(await gw.deposits(APP, hex(s.publicKey))).toEqual([]);
  mock.deposits.set(hex(s.publicKey), [{ cmx: "ab".repeat(32), value: 1_000_000, height: 2001, confirmations: 3, state: "seen" }]);
  const d = await gw.deposits(APP, hex(s.publicKey));
  expect(d[0].state).toBe("seen");
});

test("the page refuses a wrong-network payout address and a non-denomination amount", async () => {
  const gw = new Gateway(mock.url);
  const s = sessionFromSeed(new Uint8Array(32).fill(11));
  const mainnetUa = encodeOrchardUa(new Uint8Array(43).fill(0x22), "mainnet");
  await expect(prepareDeposit(gw, APP, s, mainnetUa, 1_000_000n)).rejects.toThrow(/mainnet/);
  await expect(prepareDeposit(gw, APP, s, payout, 1_234n)).rejects.toThrow(/one of/);
});

test("gateway errors surface with status and message", async () => {
  const gw = new Gateway(mock.url);
  await expect(gw.app("SomeOtherApp")).rejects.toBeInstanceOf(GatewayError);
  await expect(gw.registerIntent(APP, "00".repeat(512))).rejects.toThrow(/not a v2 memo/);
});
