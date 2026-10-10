// Drives a full coin flip through the page's own code (src/chain.ts, src/session.ts) against
// the local MagicBlock stack: base :28899, ER :27799, VRF oracle on the ER. Deposits are
// credited by the Rust harness (spikes/magicblock/ledger-e2e `coinflip credit`), standing in
// for the gateway's attestor.
//   npx vite-node scripts/e2e-local.ts
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { allowedStakes, Coinflip, decodeBalance, decodeLedgerParams, decodePot, decodeReceipt, decodeTable, DLP, sendTx } from "../src/chain";
import { sessionFromSeed } from "../src/session";

const root = new URL("../..", import.meta.url).pathname;
const BASE = process.env.BASE_RPC ?? "http://127.0.0.1:28899";
const ER = process.env.ER_RPC ?? "http://127.0.0.1:27799";
const QUEUE = new PublicKey(process.env.VRF_QUEUE ?? "Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT");
const base = new Connection(BASE, "confirmed");
const er = new Connection(ER, "confirmed");
const cf = new Coinflip({ ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"), app: new PublicKey("7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo") });
const relayer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(`${root}spikes/magicblock/keys/payer.json`, "utf8"))));
const out = (o: object) => console.log(JSON.stringify(o));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until<T>(what: string, secs: number, f: () => Promise<T | null | undefined | false>): Promise<[T, number]> {
  const t0 = Date.now();
  for (;;) {
    const v = await f().catch(() => null);
    if (v) return [v as T, Date.now() - t0];
    if (Date.now() - t0 > secs * 1000) throw new Error(`timeout: ${what}`);
    await sleep(100);
  }
}

const players = [0, 1].map(() => sessionFromSeed(crypto.getRandomValues(new Uint8Array(32))));
for (const p of players) {
  execFileSync(`${root}spikes/magicblock/target/e2e/release/coinflip`, ["credit", p.keypair.publicKey.toBase58(), "1000000"], { stdio: "inherit" });
}
const bal = async (k: PublicKey) => decodeBalance((await base.getAccountInfo(cf.pdas.balance(k)))!.data);
out({ step: "credited", balances: await Promise.all(players.map(async (p) => String((await bal(p.keypair.publicKey)).amount))) });

const potId = crypto.getRandomValues(new Uint8Array(32));
const pot = cf.pdas.pot(potId);
const table = cf.pdas.table(potId);
const identity = new PublicKey((await (await fetch(ER, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }) })).json()).result.identity);
const stake = allowedStakes(decodeLedgerParams((await base.getAccountInfo(cf.pdas.config()))!.data))[0]; // one deposit, net
out({ step: "create_table", stake: String(stake), sig: await sendTx(base, [cf.createTable(relayer.publicKey, potId, stake, 1800n, identity)], relayer) });
for (const [i, p] of players.entries()) {
  out({ step: `join p${i}`, sig: await sendTx(base, [cf.join(p.keypair.publicKey, potId)], relayer, [p.keypair]) });
}
out({ step: "start", sig: await sendTx(base, [cf.start(relayer.publicKey, potId, 600n)], relayer, [], 400_000) });
const [, visible] = await until("table in ER", 30, async () => (await er.getAccountInfo(table))?.owner.equals(cf.p.app));
out({ step: "in rollup", ms: visible, base_owner: (await base.getAccountInfo(table))!.owner.equals(DLP) ? "delegation program" : "?" });

// Rollup: the zero-SOL session key pays.
const flipper = players[0];
out({ step: "flip (session key pays, 0 SOL)", sol: await base.getBalance(flipper.keypair.publicKey), sig: await sendTx(er, [cf.flip(flipper.keypair.publicKey, potId, QUEUE, crypto.getRandomValues(new Uint8Array(32)))], flipper.keypair) });
const [landed, landMs] = await until("land", 90, async () => {
  const t = decodeTable((await er.getAccountInfo(table))!.data);
  return t.state === "Landed" ? t : null;
});
out({ step: "land (VRF oracle callback)", ms: landMs, winner: landed.winner, amounts: decodePot((await er.getAccountInfo(pot))!.data).amounts.map(String) });
const t0 = Date.now();
out({ step: "finish (session key pays)", sig: await sendTx(er, [cf.finish(flipper.keypair.publicKey, potId)], flipper.keypair) });
const [, ret] = await until("return", 120, async () => (await base.getAccountInfo(pot))?.owner.equals(cf.p.ledger) && (await base.getAccountInfo(table))?.owner.equals(cf.p.app));
out({ step: "returned", ms: ret });
const r = decodeReceipt((await base.getAccountInfo(cf.pdas.receipt(pot)))!.data);
out({ step: "cash_out", sig: await sendTx(base, [cf.cashOut(potId, r.players)], relayer), finish_to_cash_out_ms: Date.now() - t0 });
const after = await Promise.all(players.map(async (p) => (await bal(p.keypair.publicKey)).amount));
const winnerKey = r.players[landed.winner];
const expect = players.map((p) => (p.keypair.publicKey.equals(winnerKey) ? 2n * stake : 0n));
out({ step: "balances", after: after.map(String), expected: expect.map(String), match: after.every((a, i) => a === expect[i]) });

// close_table: anyone; the table's rent goes back to its creator (the relayer here).
{
  const rent = (await base.getAccountInfo(table))!.lamports;
  const before = await base.getBalance(relayer.publicKey);
  const stranger = Keypair.generate();
  await base.confirmTransaction(await base.requestAirdrop(stranger.publicKey, 1e9), "confirmed");
  await sendTx(base, [cf.closeTable(potId, relayer.publicKey)], stranger);
  out({ step: "close_table (anyone; rent to the creator)", table_rent: rent, creator_gain: (await base.getBalance(relayer.publicKey)) - before, table_gone: (await base.getAccountInfo(table)) === null });
}
const winner = players.find((p) => p.keypair.publicKey.equals(winnerKey))!;
out({ step: "request_exit winner", sig: await sendTx(base, [cf.requestExit(winner.keypair.publicKey)], relayer, [winner.keypair]), exiting: (await bal(winner.keypair.publicKey)).exiting });
const loser = players.find((p) => !p.keypair.publicKey.equals(winnerKey))!;
out({ step: "request_exit loser (balance 0)", refused: await sendTx(base, [cf.requestExit(loser.keypair.publicKey)], relayer, [loser.keypair]).then(() => false, () => true) });
