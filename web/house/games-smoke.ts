// Up/Down and the sealed-bid auction, headless, against the live house: the same endpoints and
// rollup instructions the pages use. Players are existing session seeds with ledger balances
// (e.g. from house/multi-smoke.ts runs), in a JSON array of hex seeds:
//   PLAYERS=../keys/players.json BETS=3 npx vite-node house/games-smoke.ts updown
//   PLAYERS=../keys/players.json npx vite-node house/games-smoke.ts auction   # needs 2+ players
// The auction run has the last player seal but never reveal, to exercise the penalty.
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { readFileSync } from "node:fs";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Auctions, bidCommitment, bidSalt, Coinflip, decodeBalance, decodePrice, decodeTable, sendTx, ZEC_USD_FEED } from "../src/chain";
import { sessionFromSeed, unhex, type Session } from "../src/session";


const HOST = process.env.HOST ?? "https://159-223-38-126.sslip.io";
const base = new Connection(process.env.BASE_RPC ?? "https://api.devnet.solana.com", "confirmed");
const er = new Connection(process.env.ER_RPC ?? "https://devnet-as.magicblock.app", "confirmed");
const cf = new Coinflip({ ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"), app: new PublicKey("7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo") });
const au = new Auctions(cf);
const out = (o: object) => console.log(JSON.stringify({ t: new Date().toISOString().slice(11, 19), ...o }));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function http(path: string, body?: unknown) {
  const r = await fetch(`${HOST}${path}`, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${path}: ${r.status} ${t}`);
  return JSON.parse(t);
}
async function until<T>(what: string, secs: number, f: () => Promise<T | null | undefined | false>, every = 1_000): Promise<T> {
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
const balanceOf = async (s: Session) => {
  const b = await base.getAccountInfo(cf.pdas.balance(s.keypair.publicKey));
  return b ? decodeBalance(b.data).amount : 0n;
};

const players: Session[] = (JSON.parse(readFileSync(process.env.PLAYERS ?? "", "utf8")) as string[]).map((h) => sessionFromSeed(unhex(h)));

async function updown() {
  const p = players[0];
  const me = p.keypair.publicKey.toBase58();
  const before = await balanceOf(p);
  const play = await http("/house/play", { session: me, game: "updown" });
  if (!play.resume) await http("/house/join", { game: play.game, tx: sign(play.tx, p) });
  await until("Up/Down table in the rollup", 120, async () => (await http(`/house/game/${play.game}`)).state === "in_rollup");
  const potId = Uint8Array.from(Buffer.from(play.game, "hex"));
  out({ step: "table open", game: play.game.slice(0, 16), rounds: play.rounds, balance_before: String(before) });
  for (let i = 0; i < Number(process.env.BETS ?? 2); i++) {
    const up = i % 2 === 0;
    const price = decodePrice((await er.getAccountInfo(ZEC_USD_FEED))!.data).price;
    await sendTx(er, [cf.bet(p.keypair.publicKey, potId, up)], p.keypair);
    const t = decodeTable((await er.getAccountInfo(cf.pdas.table(potId)))!.data);
    await sleep((t.bet.expiry - Date.now() / 1000 + 1) * 1000);
    // Settle like the page: retry until a post-expiry price is in (the closer may beat us).
    const r = await until("settled", 15, async () => {
      await sendTx(er, [cf.settleBet(potId)], p.keypair).catch(() => {});
      const x = decodeTable((await er.getAccountInfo(cf.pdas.table(potId)))!.data);
      return x.bet.open ? null : x;
    });
    const outcome = r.flips === t.flips ? "void" : r.wins > t.wins ? "won" : r.ties > t.ties ? "tie" : "lost";
    out({ step: `bet ${i + 1}`, call: up ? "up" : "down", seen: String(price), open: String(t.bet.openPrice), settle: String(r.bet.settlePrice), outcome });
  }
  await http("/house/stop", { session: me });
  const g = await until("table closed and paid", 180, async () => {
    const x = await http(`/house/game/${play.game}`);
    return ["done", "final"].includes(x.state) ? x : null;
  }, 3_000);
  out({ step: "closed", events: g.events.slice(-3).map((e: any) => e.step), balance_after: String(await balanceOf(p)) });
}

async function auction() {
  if (players.length < 2) throw new Error("the auction run needs 2+ players");
  const opened = (await http("/house/auction/open", {})).auction;
  const potId = Uint8Array.from(Buffer.from(opened.id, "hex"));
  const auction = au.auction(potId);
  out({ step: "auction open", number: opened.number, address: opened.address });
  const before = await Promise.all(players.map(balanceOf));
  for (const p of players) {
    const r = await http("/house/auction/register", { session: p.keypair.publicKey.toBase58() });
    await http("/house/auction/register/sign", { id: r.id, tx: sign(r.tx, p) });
  }
  await until("bidding", 180, async () => (await http("/house/auction")).auction?.chain?.state === "Bidding", 2_000);
  const bids = players.map((_, i) => BigInt(10_000 * (i + 1)));
  for (const [i, p] of players.entries()) {
    const salt = bidSalt(p.seed, auction);
    await sendTx(er, [au.commit(p.keypair.publicKey, potId, bidCommitment(auction, p.keypair.publicKey, bids[i], salt))], p.keypair);
  }
  const c = (await http("/house/auction")).auction.chain;
  out({ step: "sealed", bids: bids.map(String), committed: c.committed });
  await sleep((c.commitDeadline - Date.now() / 1000 + 1) * 1000);
  for (const [i, p] of players.slice(0, -1).entries()) {
    await sendTx(er, [au.reveal(p.keypair.publicKey, potId, bids[i], bidSalt(p.seed, auction))], p.keypair);
  }
  out({ step: "revealed all but the last" });
  const done = await until("auction paid", 240, async () => {
    const a = (await http("/house/auction")).auction;
    return a.id === opened.id && a.state === "done" ? a : null;
  }, 3_000);
  const after = await Promise.all(players.map(balanceOf));
  out({ step: "result", winner: done.chain.winnerKey, price: done.chain.price, deltas: after.map((a, i) => String(a - before[i])), events: done.events.map((e: any) => e.step) });
}

const which = process.argv.at(-1);
await (which === "auction" ? auction() : updown());
