// The game page's flow, headless: deposit (no auto-exit), open one session table and flip
// FLIPS times in the rollup while both seats cover a stake, then cash out with the house's co-signed withdrawal and follow
// it to the Zcash payout. Run from web/ with the SSH tunnel to op0's zol-zcashd up:
//   npx vite-node house/multi-smoke.ts
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { writeFileSync } from "node:fs";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Gateway } from "../src/gateway";
import { prepareDeposit } from "../src/deposit";
import { hex, sessionFromSeed, type Session } from "../src/session";
import { Coinflip, decodeBalance, decodePot, decodeTable, sendTx } from "../src/chain";

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
async function until<T>(what: string, secs: number, f: () => Promise<T | null | undefined | false>, every = 3_000): Promise<T> {
  const t0 = Date.now();
  for (;;) {
    const v = await f().catch(() => null);
    if (v) return v as T;
    if (Date.now() - t0 > secs * 1000) throw new Error(`timeout: ${what}`);
    await sleep(every);
  }
}
const sign = (b64: string, s: Session) => {
  const tx = Transaction.from(Buffer.from(b64, "base64"));
  tx.partialSign(s.keypair);
  return Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64");
};
const balanceOf = async (p: PublicKey) => {
  const b = await base.getAccountInfo(cf.pdas.balance(p));
  return b ? decodeBalance(b.data) : null;
};

const seed = crypto.getRandomValues(new Uint8Array(32));
writeFileSync(`${root}keys/multi-smoke-${new Date().toISOString().replace(/[:.]/g, "")}.json`, JSON.stringify([hex(seed)]), { mode: 0o600 });
const p = sessionFromSeed(seed);
const me = p.keypair.publicKey;
const funding = await http(`${ZCASHD}/admin/funding`);
out({ step: "player", session: me.toBase58() });

// Deposit 0.01 TAZ without auto-exit: the balance stays on the ledger between flips.
// DEPOSITS deposits of 0.01 TAZ (default 2, so a cash-out remains even if the house wins a round).
const DEPOSITS = Number(process.env.DEPOSITS ?? 2);
const FLIPS = Number(process.env.FLIPS ?? 1);
const t0 = Date.now();
for (let i = 0; i < DEPOSITS; i++) {
  const d = await prepareDeposit([new Gateway(`${HOST}/op0`)], APP, p, funding.ua, 1_000_000n, false);
  await until("funding spendable", 600, async () => (await http(`${ZCASHD}/admin/funding`)).spendable >= 1_010_000, 10_000);
  const memo = Buffer.from(new URL(d.link.replace("zcash:", "zcash://x/")).searchParams.get("memo")!, "base64url").toString("utf8");
  await http(`${ZCASHD}/admin/send`, { to: d.session.ua, value: 1_000_000, memo });
}
await until("credited", 1800, async () => ((await balanceOf(me))?.amount ?? 0n) >= BigInt(DEPOSITS) * 997_000n, 10_000);
out({ step: "credited", seconds: Math.round((Date.now() - t0) / 1000), auto_exit: (await balanceOf(me))!.autoExit });

// One session: one signed setup, then FLIPS flips in the rollup (one stake moves per flip).
const stake = BigInt((await http(`${HOST}/house/info`)).stake);
const s0 = Date.now();
const play = await http(`${HOST}/house/play`, { session: me.toBase58() });
if (!play.resume) await http(`${HOST}/house/join`, { game: play.game, tx: sign(play.tx, p) });
await until("table in the rollup", 120, async () => (await http(`${HOST}/house/game/${play.game}`)).state === "in_rollup", 1_000);
out({ step: "session open", rounds: play.rounds, stake: String(stake), seconds: Math.round((Date.now() - s0) / 1000) });
const potId = Uint8Array.from(Buffer.from(play.game, "hex"));
const rollup = async () => {
  const [t, q] = await er.getMultipleAccountsInfo([cf.pdas.table(potId), cf.pdas.pot(potId)]);
  return { t: decodeTable(t!.data), seats: decodePot(q!.data).amounts };
};
for (let round = 1; round <= FLIPS; round++) {
  const r0 = Date.now();
  const before = await rollup();
  if (before.seats[0] < stake || before.seats[1] < stake) break;
  await sendTx(er, [cf.flip(me, potId, QUEUE, crypto.getRandomValues(new Uint8Array(32)))], p.keypair);
  const after = await until("landed", 90, async () => {
    const a = await rollup();
    return a.t.flips > before.t.flips ? a : null;
  }, 300);
  out({ step: `flip ${round}`, won: after.t.winner === 1, ms: Date.now() - r0, seat: String(after.seats[1]) });
}

// Cash out whatever is left.
const left = ((await balanceOf(me))?.amount ?? 0n) + ((await rollup().catch(() => null))?.seats[1] ?? 0n);
if (left === 0n) {
  out({ step: "nothing to cash out (the house won every round)" });
} else {
  // Withdrawing closes the open table first; ask until it has.
  await http(`${HOST}/house/stop`, { session: me.toBase58() });
  const w = await until("table closed", 300, () => http(`${HOST}/house/withdraw`, { session: me.toBase58() }), 3_000);
  const signed = await http(`${HOST}/house/withdraw/sign`, { id: w.id, tx: sign(w.tx, p) });
  out({ step: "cash-out settled", amount: w.amount, settlement: w.settlement, state: signed.state });
  const paid = await until("paid on Zcash", 1800, async () => {
    const g = await http(`${HOST}/house/game/${w.id}`);
    return ["paid", "final"].includes(g.state) ? g : null;
  }, 10_000);
  out({ step: "cash-out paid on Zcash", zcash: paid.zcash, minutes_from_deposit: ((Date.now() - t0) / 60000).toFixed(1) });
}
