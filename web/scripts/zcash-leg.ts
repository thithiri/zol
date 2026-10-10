// The whole app with the Zcash leg: the page's deposit flow against the real gateway page
// API, a wallet payment simulated by zol-zcashd's dev wallet, attested credit, the coin
// flip in the rollup with VRF, cash-out, exit, settlement and payout on regtest.
// Started by spikes/magicblock/zcash-leg/run.sh, which brings everything up.
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { Gateway } from "../src/gateway";
import { prepareDeposit } from "../src/deposit";
import { hex, sessionFromSeed } from "../src/session";
import { orchardReceiverFromUa } from "../src/ua";
import { Coinflip, decodeBalance, decodePot, decodeReceipt, decodeTable, sendTx } from "../src/chain";
import vectors from "../test/ua-vectors.json";

const root = new URL("../..", import.meta.url).pathname;
const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const GW = process.env.GATEWAY ?? "http://127.0.0.1:18710";
const GWS = (process.env.GATEWAYS ?? "http://127.0.0.1:18710,http://127.0.0.1:18711,http://127.0.0.1:18712").split(",");
const ZCASHD = process.env.ZCASHD ?? "http://127.0.0.1:18310";
const ZEBRA = process.env.ZEBRA ?? "http://127.0.0.1:18232";
const BIN = `${root}spikes/magicblock/target/e2e/release/coinflip`;
const base = new Connection("http://127.0.0.1:28899", "confirmed");
const er = new Connection("http://127.0.0.1:27799", "confirmed");
const QUEUE = new PublicKey("Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT");
const cf = new Coinflip({ ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"), app: new PublicKey(APP) });
const relayer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(`${root}spikes/magicblock/keys/payer.json`, "utf8"))));
const out = (o: object) => console.log(JSON.stringify({ t: new Date().toISOString().slice(11, 19), ...o }));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const zebra = async (method: string, params: unknown[] = []) =>
  (await (await fetch(ZEBRA, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) })).json()).result;
async function post(url: string, body: unknown) {
  const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${url}: ${r.status} ${t}`);
  return JSON.parse(t);
}
async function until<T>(what: string, secs: number, f: () => Promise<T | null | undefined | false>, tick?: () => Promise<unknown>): Promise<T> {
  const t0 = Date.now();
  for (;;) {
    const v = await f().catch(() => null);
    if (v) return v as T;
    if (Date.now() - t0 > secs * 1000) throw new Error(`timeout: ${what}`);
    if (tick) await tick().catch(() => {});
    await sleep(1000);
  }
}

const gw = new Gateway(GW);
const app = await gw.app(APP);
out({ step: "gateway AppInfo", ...app });

// 1. Page: session keys, payout addresses, signed memos registered, ZIP 321 links.
const payouts = vectors.filter((v) => v.network === "regtest" && v.kind.startsWith("orchard-only")).slice(0, 2);
const players = [0, 1].map(() => sessionFromSeed(crypto.getRandomValues(new Uint8Array(32))));
const deps = [];
for (const [i, p] of players.entries()) {
  const d = await prepareDeposit(GWS.map((u) => new Gateway(u)), APP, p, payouts[i].ua, 1_000_000n);
  deps.push(d);
  out({ step: `page deposit link p${i}`, session: hex(p.publicKey), session_ua: d.session.ua, receiver_index: d.session.receiver_index, link_len: d.link.length, link: d.link.slice(0, 80) + "…", registered_at: d.registered.length });
}
const other = await new Gateway(GWS[2]).session(APP, hex(players[1].publicKey));
out({ step: "intent held by operator 2", intent_registered: other.intent_registered });

// 2. Wallet: pay each link. Player 0's wallet sends the ZOL1 text memo from the link;
// player 1's wallet drops the memo (spike 0.10), so only the registered intent binds it.
for (const [i, d] of deps.entries()) {
  const url = new URL(d.link.replace("zcash:", "zcash://x/"));
  const memo = i === 0 ? Array.from(Buffer.from(url.searchParams.get("memo")!, "base64url")) : [0xf6];
  const to = Array.from(orchardReceiverFromUa(d.session.ua, "regtest"));
  const r = await post(`${ZCASHD}/dev/send`, { to, value: 1_000_000, memo });
  out({ step: `wallet pays p${i}`, memo: i === 0 ? "ZOL1 text" : "dropped", txid: Buffer.from(r.txid).toString("hex").slice(0, 16) });
}
await zebra("generate", [10]);

// 3. Page polls: seen -> credited, and the ledger balance appears.
for (const [i, p] of players.entries()) {
  const t0 = Date.now();
  const st = await until(`p${i} credited`, 240, async () => {
    const ds = await gw.deposits(APP, hex(p.publicKey));
    return ds.find((d) => d.state === "credited") ?? null;
  }, () => zebra("generate", [1]));
  const b = decodeBalance((await base.getAccountInfo(cf.pdas.balance(p.keypair.publicKey)))!.data);
  out({ step: `p${i} credited`, ms: Date.now() - t0, deposit: st, ledger_balance: String(b.amount) });
}

// 4. The coin flip, through the page's code.
const identity = new PublicKey((await (await fetch("http://127.0.0.1:27799", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }) })).json()).result.identity);
const potId = crypto.getRandomValues(new Uint8Array(32));
const pot = cf.pdas.pot(potId);
const table = cf.pdas.table(potId);
await sendTx(base, [cf.createTable(relayer.publicKey, potId, 500_000n, 1800n, identity)], relayer);
for (const p of players) await sendTx(base, [cf.join(p.keypair.publicKey, potId)], relayer, [p.keypair]);
await sendTx(base, [cf.start(relayer.publicKey, potId, 600n)], relayer, [], 400_000);
await until("table in rollup", 30, async () => (await er.getAccountInfo(table))?.owner.equals(cf.p.app));
await sendTx(er, [cf.flip(players[0].keypair.publicKey, potId, QUEUE, crypto.getRandomValues(new Uint8Array(32)))], players[0].keypair);
const landed = await until("land", 60, async () => {
  const t = decodeTable((await er.getAccountInfo(table))!.data);
  return t.state === "Landed" ? t : null;
});
out({ step: "coin landed", winner_seat: landed.winner, er_amounts: decodePot((await er.getAccountInfo(pot))!.data).amounts.map(String) });
await sendTx(er, [cf.finish(players[1].keypair.publicKey, potId)], players[1].keypair);
await until("pot back on Solana", 60, async () => (await base.getAccountInfo(pot))?.owner.equals(cf.p.ledger));
const r = decodeReceipt((await base.getAccountInfo(cf.pdas.receipt(pot)))!.data);
await sendTx(base, [cf.cashOut(potId, r.players)], relayer);
const bal = await Promise.all(players.map(async (p) => decodeBalance((await base.getAccountInfo(cf.pdas.balance(p.keypair.publicKey)))!.data).amount));
out({ step: "cashed out", balances: bal.map(String), winner: r.players[landed.winner].toBase58() });

// 5. Exit and settle; the gateways pay the settlement on Zcash.
for (const p of players) await sendTx(base, [cf.requestExit(p.keypair.publicKey)], relayer, [p.keypair]);
const s = execFileSync(BIN, ["settle", ...players.map((p) => p.keypair.publicKey.toBase58())]).toString();
out({ step: "settled", out: s.trim().split("\n").map((l) => JSON.parse(l)) });
const t0 = Date.now();
const paid = await until("settlement Paid", 600, async () => {
  const st = JSON.parse(execFileSync(BIN, ["settlement", "0"]).toString());
  return st.state === "Paid" ? st : null;
}, () => zebra("generate", [5]));
out({ step: "settlement paid on Zcash and marked on Solana", ms: Date.now() - t0, ...paid });
for (const [i, p] of players.entries()) out({ step: `p${i} deposits view`, deposits: await gw.deposits(APP, hex(p.publicKey)) });
