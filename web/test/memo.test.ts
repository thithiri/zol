import { expect, test } from "vitest";
import { ed25519 } from "@noble/curves/ed25519";
import v from "../../crates/zol-wire/vectors/v0.3.json";
import { buildSignedMemo, canonicalMemo, zecString } from "../src/memo";
import { hex, loadOrCreateSession, sessionFromSeed, unhex } from "../src/session";
import { encodeMemoText, memoSigningMessage, depositLink } from "../src/wire";

const i = v.inputs;

test("memo built by the page matches the Rust golden vector byte for byte", () => {
  const s = sessionFromSeed(unhex(i.session_secret));
  expect(hex(s.publicKey)).toBe(i.session_key);
  const memo = buildSignedMemo({
    session: s, payoutAddress: unhex(i.payout_address), vaultAddress: unhex(i.vault_address),
    network: "testnet", expiryHeight: i.expiry_height, autoExit: i.flags === 1,
    appData: new TextEncoder().encode(i.app_data_utf8),
  });
  expect(hex(canonicalMemo(memo))).toBe(v.memo);
  expect(encodeMemoText(memo)).toBe(v.memo_text);
  expect(ed25519.verify(memo.signature, memoSigningMessage(memo, i.network, unhex(i.vault_address)), s.publicKey)).toBe(true);
});

test("signature binds the vault address and the network", () => {
  const s = sessionFromSeed(unhex(i.session_secret));
  const base = { session: s, payoutAddress: unhex(i.payout_address), vaultAddress: unhex(i.vault_address), expiryHeight: 100 };
  const a = buildSignedMemo({ ...base, network: "testnet" });
  const b = buildSignedMemo({ ...base, network: "regtest" });
  expect(hex(a.signature)).not.toBe(hex(b.signature));
  const other = new Uint8Array(43).fill(0x44);
  expect(ed25519.verify(a.signature, memoSigningMessage(a, 1, other), s.publicKey)).toBe(false);
});

test("ZIP 321 link carries the ZOL1 text memo, base64url", () => {
  const s = sessionFromSeed(unhex(i.session_secret));
  const memo = buildSignedMemo({ session: s, payoutAddress: unhex(i.payout_address), vaultAddress: unhex(i.vault_address), network: "regtest", expiryHeight: 5 });
  const link = depositLink("uregtest1abc", zecString(1_000_000n), memo);
  const q = new URL(link.replace("zcash:", "zcash://x/")).searchParams;
  expect(link.startsWith("zcash:uregtest1abc?amount=0.01&memo=")).toBe(true);
  const text = Buffer.from(q.get("memo")!, "base64url").toString("utf8");
  expect(text).toBe(encodeMemoText(memo));
  expect(link.length).toBeLessThan(600);
});

test("zatoshi to ZEC", () => {
  expect(zecString(100_000_000n)).toBe("1");
  expect(zecString(1_000_000n)).toBe("0.01");
  expect(zecString(123_456_789n)).toBe("1.23456789");
  expect(zecString(50_000n)).toBe("0.0005");
});

test("session key is created once per app and persisted", () => {
  const store = new Map<string, string>();
  const kv = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
  const a = loadOrCreateSession("AppA", kv);
  const a2 = loadOrCreateSession("AppA", kv);
  const b = loadOrCreateSession("AppB", kv);
  expect(hex(a.publicKey)).toBe(hex(a2.publicKey));
  expect(hex(a.publicKey)).not.toBe(hex(b.publicKey));
  expect(a.keypair.publicKey.toBytes()).toEqual(a.publicKey);
});
