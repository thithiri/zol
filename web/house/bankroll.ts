// Adds to the house's bankroll: N deposits of one denomination from op0's funding wallet,
// credited to the house session through the ordinary deposit path (no auto-exit, so the
// house's balance stays on the ledger). Run from the repo root with the SSH tunnel up:
//   ssh -i ~/zol -N -L 28300:127.0.0.1:18300 root@159.223.38.126 &
//   cd web && npx vite-node house/bankroll.ts -- <count>
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { readFileSync } from "node:fs";
import { Gateway } from "../src/gateway";
import { prepareDeposit } from "../src/deposit";
import { hex } from "../src/session";
import { House } from "./house";

const root = new URL("../..", import.meta.url).pathname;
const HOST = process.env.HOST ?? "https://zol.cash";
const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const ZCASHD = process.env.ZCASHD ?? "http://127.0.0.1:28300";
const count = Number(process.argv.at(-1)) || 1;
const house = House.sessionFromSeedHex(readFileSync(`${root}keys/house-session.hex`, "utf8").trim());
const out = (o: object) => console.log(JSON.stringify({ t: new Date().toISOString().slice(11, 19), ...o }));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function http(url: string, body?: unknown) {
  const r = await fetch(url, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${url}: ${r.status} ${t}`);
  return JSON.parse(t);
}

const gw = new Gateway(`${HOST}/op0`);
const funding = await http(`${ZCASHD}/admin/funding`);
out({ step: "house", session: house.keypair.publicKey.toBase58(), funding_spendable: funding.spendable, deposits: count });
for (let i = 0; i < count; i++) {
  // One memo per deposit: the session's receiver and intent are the same each time.
  const d = await prepareDeposit([gw], APP, house, funding.ua, 1_000_000n, false);
  for (;;) {
    const f = await http(`${ZCASHD}/admin/funding`);
    if (f.spendable >= 1_010_000) break;
    await sleep(10_000);
  }
  const memo = Buffer.from(new URL(d.link.replace("zcash:", "zcash://x/")).searchParams.get("memo")!, "base64url").toString("utf8");
  const r = await http(`${ZCASHD}/admin/send`, { to: d.session.ua, value: 1_000_000, memo });
  out({ step: `bankroll deposit ${i + 1}/${count}`, session_hex: hex(house.publicKey), txid: Buffer.from(r.txid).reverse().toString("hex") });
}
