// ZOL quickstart against the testnet deployment: a player key, a shielded deposit, a few coin
// flips in the MagicBlock rollup, and a withdrawal back to your Zcash wallet.
// See dev-docs/quickstart.md. Run from web/:
//
//   npx vite-node examples/quickstart.ts new                       # a player (session) key
//   npx vite-node examples/quickstart.ts deposit <your utest1… address>   # a payment link
//   npx vite-node examples/quickstart.ts status                    # deposits and balance
//   npx vite-node examples/quickstart.ts flip 3                    # a table, 3 flips, close
//   npx vite-node examples/quickstart.ts withdraw                  # back to your address
//
// The session seed is written to ./zol-session.json (mode 600). Keep it: it is the only key
// to your balance.
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Coinflip, decodeBalance, decodeTable, sendTx } from "../src/chain";
import { prepareDeposit } from "../src/deposit";
import { Gateway } from "../src/gateway";
import { hex, sessionFromSeed, unhex, type Session } from "../src/session";


// The testnet deployment (dev-docs/reference/networks.md).
const HOST = process.env.ZOL_HOST ?? "https://zol.cash";
const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo"; // the games app (coin flip, Up/Down, auction)
const LEDGER = "3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y";
const VRF_QUEUE = new PublicKey("5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc");
const base = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection("https://devnet-as.magicblock.app", "confirmed");
const gateways = ["op0", "op1", "op2"].map((op) => new Gateway(`${HOST}/${op}`));
const cf = new Coinflip({ ledger: new PublicKey(LEDGER), app: new PublicKey(APP) });
const FILE = process.env.SESSION_FILE ?? "zol-session.json";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const taz = (zat: bigint) => `${Number(zat) / 1e8} TAZ`;
async function house(path: string, body?: unknown) {
  const r = await fetch(`${HOST}/house${path}`, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(`${path}: ${j.error ?? r.status}`);
  return j;
}
function load(): Session {
  if (!existsSync(FILE)) throw new Error(`no ${FILE}: run "new" first`);
  return sessionFromSeed(unhex(JSON.parse(readFileSync(FILE, "utf8")).seed));
}
/** The player signs a transaction the house built (the house pays the fee and co-signs). */
function sign(b64: string, s: Session) {
  const tx = Transaction.from(Buffer.from(b64, "base64"));
  tx.partialSign(s.keypair);
  return Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64");
}
async function balance(s: Session) {
  const a = await base.getAccountInfo(cf.pdas.balance(s.keypair.publicKey));
  return a ? decodeBalance(a.data) : null;
}

const [cmd, arg] = process.argv.slice(2).filter((a) => a !== "--");
switch (cmd) {
  case "new": {
    if (existsSync(FILE)) throw new Error(`${FILE} exists: refusing to overwrite a key that may hold a balance`);
    const seed = crypto.getRandomValues(new Uint8Array(32));
    writeFileSync(FILE, JSON.stringify({ seed: hex(seed) }), { mode: 0o600 });
    console.log(`player key ${sessionFromSeed(seed).keypair.publicKey.toBase58()} saved in ${FILE}`);
    break;
  }
  case "deposit": {
    if (!arg) throw new Error("usage: deposit <your Zcash testnet unified address, utest1…>");
    const s = load();
    // Registers the intent (memo signed by your key, naming your payout address) with every
    // operator, and returns a ZIP 321 link to pay from any NU7 testnet wallet.
    const d = await prepareDeposit(gateways, APP, s, arg, 1_000_000n);
    console.log(`pay exactly ${taz(d.amountZat)} with this link (any Zcash testnet wallet):\n\n${d.link}\n`);
    console.log(`session address: ${d.session.ua}\nregistered with ${d.registered.length} of ${gateways.length} operators`);
    break;
  }
  case "status": {
    const s = load();
    const deposits = await gateways[0].deposits(APP, hex(s.publicKey)).catch(() => []);
    for (const d of deposits) console.log(`deposit ${taz(BigInt(d.value))}: ${d.state}, ${d.confirmations} confirmations`);
    const b = await balance(s);
    console.log(`balance: ${b ? taz(b.amount) : "none yet"}${b?.exiting ? " (being paid out)" : ""}`);
    break;
  }
  case "flip": {
    const s = load();
    const me = s.keypair.publicKey.toBase58();
    // 1. A table with the house: one transaction creates it, seats both of you with up to 10
    //    stakes each and moves it into the rollup. You sign; the house pays and co-signs.
    const p = await house("/play", { session: me, game: "coinflip" });
    if (!p.resume) await house("/join", { game: p.game, tx: sign(p.tx, s) });
    for (let g = await house(`/game/${p.game}`); g.state !== "in_rollup"; g = await house(`/game/${p.game}`)) await sleep(1_000);
    const potId = Uint8Array.from(Buffer.from(p.game, "hex"));
    const table = cf.pdas.table(potId);
    // 2. Flips run in the rollup: free, about a second each. The VRF decides; seat 1 is you.
    for (let i = 0; i < Number(arg ?? 3); i++) {
      const before = decodeTable((await er.getAccountInfo(table))!.data);
      await sendTx(er, [cf.flip(s.keypair.publicKey, potId, VRF_QUEUE, crypto.getRandomValues(new Uint8Array(32)))], s.keypair);
      let t = before;
      while (t.flips === before.flips) {
        await sleep(400);
        t = decodeTable((await er.getAccountInfo(table))!.data);
      }
      console.log(`flip ${i + 1}: ${t.winner === 1 ? "you won" : "the house won"}`);
    }
    // 3. Close the table: it returns to Solana and your seat is paid into your balance.
    await house("/stop", { session: me });
    for (let g = await house(`/game/${p.game}`); !["done", "final"].includes(g.state); g = await house(`/game/${p.game}`)) await sleep(2_000);
    console.log(`table closed; balance ${taz((await balance(s))?.amount ?? 0n)}`);
    break;
  }
  case "withdraw": {
    const s = load();
    // request_exit + settle, signed by your key; the operators then pay your balance (less the
    // 0.3% exit fee and the network fee) to the address fixed at your first deposit.
    const w = await house("/withdraw", { session: s.keypair.publicKey.toBase58() });
    await house("/withdraw/sign", { id: w.id, tx: sign(w.tx, s) });
    console.log(`withdrawing ${taz(BigInt(w.amount))} (settlement ${w.settlement}); waiting for the Zcash payout…`);
    for (let g = await house(`/game/${w.id}`); !["paid", "final"].includes(g.state); g = await house(`/game/${w.id}`)) await sleep(10_000);
    console.log("paid on Zcash: check your wallet");
    break;
  }
  default:
    console.log("commands: new | deposit <utest1…> | status | flip [n] | withdraw");
}
