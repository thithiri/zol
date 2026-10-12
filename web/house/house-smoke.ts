// One full game against the house, the way house.html plays it: deposit with auto-exit
// (paid from op0's funding wallet through the SSH tunnel), wait for credit, ask the house to
// play, sign the create+join transaction, flip in the rollup, follow the timeline to the
// Zcash payout. Run from web/ with the tunnel up (see house/bankroll.ts):
//   npx vite-node house/house-smoke.ts
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { readFileSync, writeFileSync } from "node:fs";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Gateway } from "../src/gateway";
import { prepareDeposit } from "../src/deposit";
import { hex, sessionFromSeed } from "../src/session";
import { Coinflip, decodeBalance, sendTx } from "../src/chain";

const root = new URL("../..", import.meta.url).pathname;
const HOST = process.env.HOST ?? "https://zol.cash";
const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const ZCASHD = process.env.ZCASHD ?? "http://127.0.0.1:28300";
const base = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection("https://devnet-as.magicblock.app", "confirmed");
const QUEUE = new PublicKey("5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc");
const cf = new Coinflip({ ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"), app: new PublicKey(APP) });
const out = (o: object) => console.log(JSON.stringify({ t: new Date().toISOString().slice(11, 19), ...o }));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function http(url: string, body?: unknown) {
  const r = await fetch(url, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${url}: ${r.status} ${t}`);
  return JSON.parse(t);
}
async function until<T>(what: string, secs: number, f: () => Promise<T | null | undefined | false>, every = 5_000): Promise<T> {
  const t0 = Date.now();
  for (;;) {
    const v = await f().catch(() => null);
    if (v) return v as T;
    if (Date.now() - t0 > secs * 1000) throw new Error(`timeout: ${what}`);
    await sleep(every);
  }
}

const seed = crypto.getRandomValues(new Uint8Array(32));
const seedFile = `${root}keys/house-smoke-${new Date().toISOString().replace(/[:.]/g, "")}.json`;
writeFileSync(seedFile, JSON.stringify([hex(seed)]), { mode: 0o600 });
const p = sessionFromSeed(seed);
const me = p.keypair.publicKey;
out({ step: "player session", session: me.toBase58(), seed_file: seedFile, house: await http(`${HOST}/house/info`) });

// 1. Deposit with auto-exit; the funding wallet is the player's payout address.
const funding = await http(`${ZCASHD}/admin/funding`);
const d = await prepareDeposit([new Gateway(`${HOST}/op0`)], APP, p, funding.ua, 1_000_000n, true);
await until("funding spendable", 600, async () => (await http(`${ZCASHD}/admin/funding`)).spendable >= 1_010_000, 10_000);
const memo = Buffer.from(new URL(d.link.replace("zcash:", "zcash://x/")).searchParams.get("memo")!, "base64url").toString("utf8");
const paid = await http(`${ZCASHD}/admin/send`, { to: d.session.ua, value: 1_000_000, memo });
const t0 = Date.now();
out({ step: "deposit sent", txid: Buffer.from(paid.txid).reverse().toString("hex") });
const bal = await until("credited", 1800, async () => {
  const b = await base.getAccountInfo(cf.pdas.balance(me));
  return b && decodeBalance(b.data).amount > 0n ? decodeBalance(b.data) : null;
}, 10_000);
out({ step: "credited", seconds: Math.round((Date.now() - t0) / 1000), balance: String(bal.amount), auto_exit: bal.autoExit });

// 2. Play: the house builds create+join, the player signs, the house co-signs and sends.
await until("house can play", 900, async () => (await http(`${HOST}/house/info`)).can_play, 15_000);
const play = await http(`${HOST}/house/play`, { session: me.toBase58() });
const tx = Transaction.from(Buffer.from(play.tx, "base64"));
tx.partialSign(p.keypair);
const joined = await http(`${HOST}/house/join`, { game: play.game, tx: Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64") });
out({ step: "joined", game: play.game, state: joined.state });

// 3. Flip once the table is in the rollup, then follow the house's timeline.
let shown = 0;
let flipped = false;
const final = await until("game paid on Zcash", 3600, async () => {
  const g = await http(`${HOST}/house/game/${play.game}`);
  for (const e of g.events.slice(shown)) out({ step: `house: ${e.step}`, chain: e.chain, sig: e.sig?.slice(0, 20) });
  shown = g.events.length;
  if (g.state === "in_rollup" && !flipped) {
    flipped = true;
    const sig = await sendTx(er, [cf.flip(me, Uint8Array.from(Buffer.from(play.game, "hex")), QUEUE, crypto.getRandomValues(new Uint8Array(32)))], p.keypair);
    out({ step: "player flipped", sig: sig.slice(0, 20) });
  }
  if (["failed", "expired"].includes(g.state)) throw Object.assign(new Error(g.error ?? g.state), { fatal: true });
  return ["paid", "final", "done"].includes(g.state) ? g : null;
}, 3_000).catch((e) => { throw e; });
out({ step: "result", state: final.state, player_won: final.playerWon, settlement: final.settlement, zcash: final.zcash, minutes: ((Date.now() - t0) / 60000).toFixed(1) });
