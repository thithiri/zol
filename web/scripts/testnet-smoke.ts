// The first game on the public testnet trial: Zcash testnet, Solana devnet, MagicBlock devnet.
// The page's own deposit code against the operators' public page API; two deposits paid from
// op0's funding wallet (one with its memo, one with the memo dropped); credit at early depth;
// the coin flip in the rollup with the VRF; cash-out, exit and settlement. It stops once the
// payout is broadcast on Zcash; `mark_paid` follows at payout depth (about 2 h).
//
// Needs, from the repo root:
//   ssh -i ~/zol -N -L 28300:127.0.0.1:18300 root@159.223.38.126 &   # op0 zol-zcashd /admin
//   (cd spikes/magicblock/ledger-e2e && cargo build --release --target-dir ../target/e2e --bin devnet)
//   cd web && npx vite-node scripts/testnet-smoke.ts
// Fee payer for Solana devnet: keys/page-fee-payer.json. Payouts go back to the funding wallet.
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { Gateway } from "../src/gateway";
import { prepareDeposit } from "../src/deposit";
import { hex, sessionFromSeed } from "../src/session";
import { allowedStakes, Coinflip, decodeBalance, decodeLedgerParams, decodePot, decodeReceipt, decodeTable, sendTx } from "../src/chain";

const root = new URL("../..", import.meta.url).pathname;
const HOST = process.env.HOST ?? "https://zol.cash";
const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const GWS = [0, 1, 2].map((i) => `${HOST}/op${i}`);
const ZCASHD = process.env.ZCASHD ?? "http://127.0.0.1:28300"; // op0 zol-zcashd through the SSH tunnel
const ER_URL = process.env.ER_RPC ?? "https://devnet-as.magicblock.app";
const base = new Connection(process.env.BASE_RPC ?? "https://api.devnet.solana.com", "confirmed");
const er = new Connection(ER_URL, "confirmed");
const QUEUE = new PublicKey("5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc"); // DEFAULT_EPHEMERAL_QUEUE
const DEVNET = `${root}spikes/magicblock/target/e2e/release/devnet`;
const FEE_PAYER_FILE = `${root}keys/page-fee-payer.json`;
const cf = new Coinflip({ ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"), app: new PublicKey(APP) });
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(FEE_PAYER_FILE, "utf8"))));
const out = (o: object) => console.log(JSON.stringify({ t: new Date().toISOString().slice(11, 19), ...o }));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function http(url: string, body?: unknown) {
  const r = await fetch(url, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${url}: ${r.status} ${t}`);
  return JSON.parse(t);
}
async function until<T>(what: string, secs: number, f: () => Promise<T | null | undefined | false>, every = 10_000): Promise<T> {
  const t0 = Date.now();
  for (;;) {
    const v = await f().catch(() => null);
    if (v) return v as T;
    if (Date.now() - t0 > secs * 1000) throw new Error(`timeout: ${what}`);
    await sleep(every);
  }
}

// 0. Preconditions.
const gws = GWS.map((u) => new Gateway(u));
const app = await gws[0].app(APP);
out({ step: "AppInfo (op0)", state: app.state, network: app.network, tip: app.tip, operators: app.operators, denominations: app.denominations });
if (app.state !== "active") throw new Error(`app is ${app.state}; wait for activation`);
const funding = await http(`${ZCASHD}/admin/funding`);
out({ step: "funding wallet", spendable: funding.spendable, ua: funding.ua });
if (funding.balance < 2_100_000) throw new Error("funding wallet needs at least 0.021 TAZ");
// Change from an earlier send is spendable once its transaction is mined.
await until("funding wallet spendable", 600, async () => (await http(`${ZCASHD}/admin/funding`)).spendable >= 2_100_000, 10_000);
out({ step: "fee payer", key: payer.publicKey.toBase58(), sol: (await base.getBalance(payer.publicKey)) / 1e9 });

// 1. Page: two sessions; intents registered with op0 only (gossip carries them to op1, op2).
// Session seeds are written to keys/ before any money moves: a credited balance can only be
// played, exited or recovered with its session key.
const seeds = [0, 1].map(() => crypto.getRandomValues(new Uint8Array(32)));
const seedFile = `${root}keys/smoke-sessions-${new Date().toISOString().replace(/[:.]/g, "")}.json`;
writeFileSync(seedFile, JSON.stringify(seeds.map((x) => hex(x))), { mode: 0o600 });
out({ step: "session seeds saved", file: seedFile });
const players = seeds.map((x) => sessionFromSeed(x));
const deps = [];
for (const [i, p] of players.entries()) {
  const d = await prepareDeposit([gws[0]], APP, p, funding.ua, 1_000_000n);
  deps.push(d);
  out({ step: `p${i} deposit link`, session: hex(p.publicKey), session_ua: d.session.ua });
}
for (const i of [1, 2]) {
  const s = await until(`intent gossiped to op${i}`, 120, async () => {
    const s = await gws[i].session(APP, hex(players[1].publicKey));
    return s.intent_registered ? s : null;
  }, 3_000);
  out({ step: `op${i} has p1's intent (gossip)`, intent_registered: s.intent_registered });
}

// 2. Wallet: pay each session's address from the funding wallet. p0 keeps the ZOL1 text memo
// from the link; p1's memo is dropped, so only the registered intent binds the deposit.
for (const [i, d] of deps.entries()) {
  // The wallet may hold a single note: wait until the change from the last send is mined.
  await until(`funding spendable for p${i}`, 600, async () => (await http(`${ZCASHD}/admin/funding`)).spendable >= 1_010_000, 10_000);
  const link = new URL(d.link.replace("zcash:", "zcash://x/"));
  const memo = i === 0 ? Buffer.from(link.searchParams.get("memo")!, "base64url").toString("utf8") : undefined;
  const r = await http(`${ZCASHD}/admin/send`, { to: d.session.ua, value: 1_000_000, ...(memo ? { memo } : {}) });
  out({ step: `wallet pays p${i}`, memo: memo ? "ZOL1 text" : "dropped", txid: Buffer.from(r.txid).reverse().toString("hex") });
}

// 3. Credit at early depth (30 x 25 s blocks), as the page polls it.
for (const [i, p] of players.entries()) {
  const t0 = Date.now();
  const st = await until(`p${i} credited`, 3600, async () => (await gws[0].deposits(APP, hex(p.publicKey))).find((d) => d.state === "credited") ?? null, 15_000);
  const b = decodeBalance((await base.getAccountInfo(cf.pdas.balance(p.keypair.publicKey)))!.data);
  out({ step: `p${i} credited`, min: ((Date.now() - t0) / 60000).toFixed(1), confirmations: st.confirmations, ledger_balance: String(b.amount) });
}

// 4. The coin flip, through the page's code, on MagicBlock devnet.
const identity = new PublicKey((await (await fetch(ER_URL, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }) })).json()).result.identity);
const potId = crypto.getRandomValues(new Uint8Array(32));
const pot = cf.pdas.pot(potId);
const table = cf.pdas.table(potId);
const stake = allowedStakes(decodeLedgerParams((await base.getAccountInfo(cf.pdas.config()))!.data))[0];
await sendTx(base, [cf.createTable(payer.publicKey, potId, stake, 1800n, identity)], payer);
out({ step: "table created", stake: String(stake), validator: identity.toBase58(), pot_id: hex(potId) });
for (const p of players) await sendTx(base, [cf.join(p.keypair.publicKey, potId)], payer, [p.keypair]);
await sendTx(base, [cf.start(payer.publicKey, potId, 600n)], payer, [], 400_000);
await until("table in rollup", 120, async () => (await er.getAccountInfo(table))?.owner.equals(cf.p.app), 2_000);
const tFlip = Date.now();
await sendTx(er, [cf.flip(players[0].keypair.publicKey, potId, QUEUE, crypto.getRandomValues(new Uint8Array(32)))], players[0].keypair);
const landed = await until("land", 180, async () => {
  const t = decodeTable((await er.getAccountInfo(table))!.data);
  return t.state === "Landed" ? t : null;
}, 1_000);
out({ step: "coin landed", flip_to_land_ms: Date.now() - tFlip, winner_seat: landed.winner, er_amounts: decodePot((await er.getAccountInfo(pot))!.data).amounts.map(String) });
const tFinish = Date.now();
await sendTx(er, [cf.finish(players[1].keypair.publicKey, potId)], players[1].keypair);
await until("pot back on Solana", 180, async () => (await base.getAccountInfo(pot))?.owner.equals(cf.p.ledger), 2_000);
out({ step: "returned", finish_to_return_ms: Date.now() - tFinish });
const r = decodeReceipt((await base.getAccountInfo(cf.pdas.receipt(pot)))!.data);
await sendTx(base, [cf.cashOut(potId, r.players)], payer);
const bal = await Promise.all(players.map(async (p) => decodeBalance((await base.getAccountInfo(cf.pdas.balance(p.keypair.publicKey)))!.data).amount));
out({ step: "cashed out", balances: bal.map(String), winner: r.players[landed.winner].toBase58() });
await sendTx(base, [cf.closeTable(potId, payer.publicKey)], payer);
out({ step: "table closed (rent back to the fee payer)", table_gone: (await base.getAccountInfo(table)) === null });

// 5. Exit and settle; the operators pay the settlement on Zcash to the funding wallet.
const winner = players.find((p) => p.keypair.publicKey.equals(r.players[landed.winner]))!;
await sendTx(base, [cf.requestExit(winner.keypair.publicKey)], payer, [winner.keypair]);
const s = execFileSync(DEVNET, ["settle", "--payer", FEE_PAYER_FILE, winner.keypair.publicKey.toBase58()]).toString();
out({ step: "settled", out: s.trim().split("\n").map((l) => JSON.parse(l)) });
const seq = JSON.parse(s.trim().split("\n").at(-1)!).settlement as number;
const genesis = JSON.parse(execFileSync(DEVNET, ["status"]).toString().trim().split("\n")[1]).genesis_head as string;
const appHex = Buffer.from(new PublicKey(APP).toBytes()).toString("hex");
const t0 = Date.now();
// The head link whose paid_count passes this settlement's sequence is the transition that pays it.
const head = await until(`payout of settlement ${seq} on Zcash`, 3600, async () => {
  const h = await http(`${ZCASHD}/head?app=${appHex}&genesis=${genesis}`);
  return h.chain?.some((l: any) => l.paid_count > seq) ? h : null;
}, 20_000);
const link = head.chain.find((l: any) => l.paid_count > seq);
out({ step: "payout mined on Zcash", min: ((Date.now() - t0) / 60000).toFixed(1), height: link.height, head_value: link.value, paid_count: link.paid_count, fault: head.fault ?? null });
out({ step: "next", note: `mark_paid follows at payout depth (300 blocks, about 2 h): ${DEVNET} settlement ${seq}` });
