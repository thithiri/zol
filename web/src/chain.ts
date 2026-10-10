// Solana side of the page: instruction builders for zol-coinflip and the ledger calls a
// player makes (cash_out, request_exit), account decoders, and sending to Solana or the
// rollup. Account orders follow idl/zol_coinflip.json and idl/zol_ledger.json.
import { sha256 } from "@noble/hashes/sha256";
import {
  ComputeBudgetProgram, Connection, Keypair, PublicKey, SystemProgram, SYSVAR_SLOT_HASHES_PUBKEY,
  Transaction, TransactionInstruction,
} from "@solana/web3.js";

export const DLP = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
export const MAGIC_PROGRAM = new PublicKey("Magic11111111111111111111111111111111111111");
export const MAGIC_CONTEXT = new PublicKey("MagicContext1111111111111111111111111111111");
export const VRF_PROGRAM = new PublicKey("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz");
export const MAX_SEATS = 16;
/** Stakes `create_table` accepts: one deposit's net credited value, for each denomination
 *  the ledger permits (Config.params.denominations, nonzero), at the ledger's entry fee. */
export const allowedStakes = (p: LedgerParams) =>
  p.denominations.filter((d) => d > 0n).map((d) => d - (d * BigInt(p.entryFeeBps)) / 10_000n);

const enc = new TextEncoder();
const disc = (kind: "global" | "account", name: string) => sha256(enc.encode(`${kind}:${name}`)).subarray(0, 8);
const u64le = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, true);
  return b;
};
const i64le = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigInt64(0, n, true);
  return b;
};
const cat = (...p: Uint8Array[]) => {
  const out = new Uint8Array(p.reduce((a, x) => a + x.length, 0));
  let i = 0;
  for (const x of p) {
    out.set(x, i);
    i += x.length;
  }
  return out;
};

export interface Programs {
  ledger: PublicKey;
  app: PublicKey; // zol-coinflip
}

export class Pdas {
  constructor(readonly p: Programs) {}
  private pda(seeds: Uint8Array[], program: PublicKey) {
    return PublicKey.findProgramAddressSync(seeds, program)[0];
  }
  config() { return this.pda([enc.encode("config")], this.p.ledger); }
  appAuthority() { return this.pda([enc.encode("zol-app")], this.p.app); }
  app() { return this.pda([enc.encode("app"), this.p.app.toBytes()], this.p.ledger); }
  balance(session: PublicKey) { return this.pda([enc.encode("balance"), this.app().toBytes(), session.toBytes()], this.p.ledger); }
  pot(potId: Uint8Array) { return this.pda([enc.encode("pot"), this.app().toBytes(), potId], this.p.ledger); }
  receipt(pot: PublicKey) { return this.pda([enc.encode("receipt"), pot.toBytes()], this.p.ledger); }
  table(potId: Uint8Array) { return this.pda([enc.encode("table"), potId], this.p.app); }
  identity() { return this.pda([enc.encode("identity")], this.p.app); }
  rentVault() { return this.pda([enc.encode("rent-vault")], this.p.app); }
  dlp(tag: string, acc: PublicKey) { return this.pda([enc.encode(tag), acc.toBytes()], DLP); }
  buffer(acc: PublicKey, owner: PublicKey) { return this.pda([enc.encode("buffer"), acc.toBytes()], owner); }
}

const m = (pubkey: PublicKey, isWritable = false, isSigner = false) => ({ pubkey, isWritable, isSigner });

export class Coinflip {
  readonly pdas: Pdas;
  constructor(readonly p: Programs) {
    this.pdas = new Pdas(p);
  }
  private ix(keys: ReturnType<typeof m>[], data: Uint8Array, program = this.p.app) {
    return new TransactionInstruction({ programId: program, keys, data: Buffer.from(data) });
  }

  createTable(payer: PublicKey, potId: Uint8Array, stake: bigint, openSecs: bigint, validator: PublicKey) {
    const d = this.pdas;
    const pot = d.pot(potId);
    return this.ix(
      [m(payer, true, true), m(d.table(potId), true), m(d.appAuthority()), m(d.config()), m(d.app(), true), m(pot, true),
        m(d.receipt(pot), true), m(this.p.ledger), m(SystemProgram.programId)],
      cat(disc("global", "create_table"), potId, u64le(stake), i64le(openSecs), validator.toBytes()),
    );
  }

  join(session: PublicKey, potId: Uint8Array) {
    const d = this.pdas;
    const pot = d.pot(potId);
    return this.ix(
      [m(session, false, true), m(d.table(potId)), m(d.appAuthority()), m(d.config()), m(d.app(), true),
        m(d.balance(session), true), m(pot, true), m(d.receipt(pot), true), m(this.p.ledger)],
      disc("global", "join"),
    );
  }

  start(payer: PublicKey, potId: Uint8Array, sessionSecs: bigint) {
    const d = this.pdas;
    const pot = d.pot(potId);
    const table = d.table(potId);
    return this.ix(
      [m(payer, true, true), m(d.buffer(table, this.p.app), true), m(d.dlp("delegation", table), true),
        m(d.dlp("delegation-metadata", table), true), m(table, true), m(d.appAuthority()), m(d.config()), m(d.app(), true),
        m(d.receipt(pot), true), m(pot, true), m(d.buffer(pot, this.p.ledger), true), m(d.dlp("delegation", pot), true),
        m(d.dlp("delegation-metadata", pot), true), m(this.p.ledger), m(d.rentVault(), true), m(this.p.app), m(DLP), m(SystemProgram.programId)],
      cat(disc("global", "start"), i64le(sessionSecs)),
    );
  }

  flip(payer: PublicKey, potId: Uint8Array, queue: PublicKey, callerSeed: Uint8Array) {
    const d = this.pdas;
    return this.ix(
      [m(payer, true, true), m(d.table(potId), true), m(queue, true), m(d.identity()), m(VRF_PROGRAM),
        m(SYSVAR_SLOT_HASHES_PUBKEY), m(SystemProgram.programId)],
      cat(disc("global", "flip"), callerSeed),
    );
  }

  finish(payer: PublicKey, potId: Uint8Array) {
    const d = this.pdas;
    return this.ix(
      [m(payer, true, true), m(d.table(potId), true), m(d.pot(potId), true), m(d.appAuthority()), m(this.p.ledger),
        m(MAGIC_PROGRAM), m(MAGIC_CONTEXT, true)],
      disc("global", "finish"),
    );
  }

  /** Anyone, once the pot is cashed out or refunded: the table's rent goes to its creator. */
  closeTable(potId: Uint8Array, creator: PublicKey) {
    const d = this.pdas;
    return this.ix([m(d.table(potId), true), m(d.receipt(d.pot(potId))), m(creator, true)], disc("global", "close_table"));
  }

  /** Anyone, for a table stuck delegated once the ledger refunded its pot: first call
   *  requests undelegation, a call after the 9,000-slot timeout rolls the table back. */
  rescueTable(potId: Uint8Array, validator: PublicKey) {
    const d = this.pdas;
    const t = d.table(potId);
    return this.ix(
      [m(t, true), m(d.receipt(d.pot(potId))), m(d.rentVault(), true), m(d.dlp("undelegation-request", t), true),
        m(d.dlp("delegation", t), true), m(d.dlp("delegation-metadata", t), true), m(d.dlp("state-diff", t), true),
        m(d.dlp("commit-state-record", t), true), m(validator, true), m(this.p.app), m(DLP), m(SystemProgram.programId)],
      disc("global", "rescue_table"),
    );
  }

  /** Ledger `cash_out`: anyone; balances in receipt order. */
  cashOut(potId: Uint8Array, players: PublicKey[]) {
    const d = this.pdas;
    const pot = d.pot(potId);
    return this.ix(
      [m(d.config()), m(d.app()), m(d.receipt(pot), true), m(pot), ...players.map((p) => m(d.balance(p), true))],
      disc("global", "cash_out"),
      this.p.ledger,
    );
  }

  /** Ledger `request_exit`: the session key signs. */
  requestExit(session: PublicKey) {
    const d = this.pdas;
    return this.ix([m(session, false, true), m(d.config()), m(d.balance(session), true)], disc("global", "request_exit"), this.p.ledger);
  }
}

// ---------------------------------------------------------------- decoders

class Reader {
  private dv: DataView;
  constructor(private b: Uint8Array, public o = 8) {
    this.dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  }
  key() { const k = new PublicKey(this.b.subarray(this.o, this.o + 32)); this.o += 32; return k; }
  bytes(n: number) { const x = this.b.slice(this.o, this.o + n); this.o += n; return x; }
  u8() { return this.b[this.o++]; }
  u64() { const v = this.dv.getBigUint64(this.o, true); this.o += 8; return v; }
  i64() { const v = this.dv.getBigInt64(this.o, true); this.o += 8; return v; }
}

function checkDisc(data: Uint8Array, name: string) {
  const d = disc("account", name);
  if (!d.every((x, i) => data[i] === x)) throw new Error(`not a ${name} account`);
}

export const TABLE_STATES = ["Open", "Started", "Flipping", "Landed", "Finished"] as const;
export const POT_STATES = ["Open", "Delegated", "CashedOut", "Refunded"] as const;

export interface TableAcc { pot: PublicKey; potId: Uint8Array; stake: bigint; validator: PublicKey; creator: PublicKey; state: (typeof TABLE_STATES)[number]; winner: number }
export function decodeTable(data: Uint8Array): TableAcc {
  checkDisc(data, "Table");
  const r = new Reader(data);
  return { pot: r.key(), potId: r.bytes(32), stake: r.u64(), validator: r.key(), creator: r.key(), state: TABLE_STATES[r.u8()], winner: r.u8() };
}

export interface ReceiptAcc { state: (typeof POT_STATES)[number]; players: PublicKey[]; buyIns: bigint[]; total: bigint; validator: PublicKey }
export function decodeReceipt(data: Uint8Array): ReceiptAcc {
  checkDisc(data, "PotReceipt");
  const r = new Reader(data);
  r.key(); r.key(); r.bytes(32);
  const state = POT_STATES[r.u8()];
  const n = r.u8();
  const players = Array.from({ length: MAX_SEATS }, () => r.key()).slice(0, n);
  const buyIns = Array.from({ length: MAX_SEATS }, () => r.u64()).slice(0, n);
  const total = r.u64();
  r.i64(); r.i64();
  return { state, players, buyIns, total, validator: r.key() };
}

export interface PotAcc { seatCount: number; amounts: bigint[]; delegated: boolean }
export function decodePot(data: Uint8Array): PotAcc {
  checkDisc(data, "Pot");
  const r = new Reader(data);
  r.key(); r.key(); r.bytes(32);
  const n = r.u8();
  const amounts = Array.from({ length: MAX_SEATS }, () => r.u64()).slice(0, n);
  return { seatCount: n, amounts, delegated: r.u8() === 1 };
}

export interface LedgerParams { entryFeeBps: number; denominations: bigint[] }
/** Ledger Config params the page needs. `params` starts after admin, attestor, 7 guardians
 *  and 4 one-byte fields; `denominations` follows 2 u16, 2 u64, a u8, 5 i64 and a u64. */
export function decodeLedgerParams(data: Uint8Array): LedgerParams {
  checkDisc(data, "Config");
  const p = 8 + 32 + 32 + 7 * 32 + 4;
  const dv = new DataView(data.buffer, data.byteOffset);
  const d0 = p + 2 + 2 + 8 + 8 + 1 + 5 * 8 + 8;
  return { entryFeeBps: dv.getUint16(p, true), denominations: [0, 1, 2, 3].map((i) => dv.getBigUint64(d0 + 8 * i, true)) };
}

export interface BalanceAcc { amount: bigint; exiting: boolean; autoExit: boolean }
export function decodeBalance(data: Uint8Array): BalanceAcc {
  checkDisc(data, "Balance");
  const r = new Reader(data);
  r.key(); r.key();
  const amount = r.u64();
  r.bytes(32);
  return { amount, exiting: r.u8() === 1, autoExit: r.u8() === 1 };
}

// ---------------------------------------------------------------- sending

export async function sendTx(conn: Connection, ixs: TransactionInstruction[], feePayer: Keypair, signers: Keypair[] = [], cuLimit?: number) {
  const tx = new Transaction();
  if (cuLimit) tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cuLimit }));
  tx.add(...ixs);
  tx.feePayer = feePayer.publicKey;
  const bh = await conn.getLatestBlockhash("confirmed");
  tx.recentBlockhash = bh.blockhash;
  const all = [feePayer, ...signers.filter((s) => !s.publicKey.equals(feePayer.publicKey))];
  tx.sign(...all);
  const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true });
  const res = await conn.confirmTransaction({ signature: sig, ...bh }, "confirmed");
  if (res.value.err) {
    const t = await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }).catch(() => null);
    throw new Error(`${JSON.stringify(res.value.err)}\n${(t?.meta?.logMessages ?? []).slice(-8).join("\n")}`);
  }
  return sig;
}
