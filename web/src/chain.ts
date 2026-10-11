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
/** MagicBlock's price oracle and its ZEC/USD feed (Pyth Lazer id 66), live in the rollup only. */
export const PRICE_PROGRAM = new PublicKey("PriCems5tHihc6UDXDjzjeawomAwBduWMGAi8ZUjppd");
export const ZEC_USD_FEED = new PublicKey("6XWQr2Y1XEpJrCdVbYGupnDeb3wkR4YXazVXdgn2Lwpg");
/** Up/Down terms (zol-coinflip constants). */
export const UPDOWN = { payoutBps: 19_000n, roundSecs: 30, settleWindowSecs: 10, publishWindowSecs: 2 } as const;
/** The oracle account: price (8 decimals) and publish time (unix s). */
export function decodePrice(data: Uint8Array) {
  const dv = new DataView(data.buffer, data.byteOffset, data.byteLength);
  return { price: dv.getBigInt64(73, true), published: Number(dv.getBigInt64(93, true)) };
}
/** MagicBlock's fee vault for a validator: a delegated payer's commit fees go there. */
export const magicFeeVault = (validator: PublicKey) =>
  PublicKey.findProgramAddressSync([new TextEncoder().encode("magic-fee-vault"), validator.toBytes()], DLP)[0];
/** A deposit also splits into this many smaller stakes (zol-coinflip STAKE_SPLIT). */
export const STAKE_SPLIT = 10n;
/** Stakes `create_table` accepts: one deposit's net credited value, for each denomination
 *  the ledger permits (Config.params.denominations, nonzero), at the ledger's entry fee;
 *  then a tenth of each. Whole-deposit stakes first; no duplicates. */
export const allowedStakes = (p: LedgerParams) => {
  const net = p.denominations.filter((d) => d > 0n).map((d) => d - (d * BigInt(p.entryFeeBps)) / 10_000n);
  return [...new Set([...net, ...net.map((n) => n / STAKE_SPLIT).filter((s) => s > 0n)])];
};

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
  pda(seeds: Uint8Array[], program: PublicKey) {
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

  /** A coin-flip table, or (`updown`) a ZEC Up/Down table. */
  createTable(payer: PublicKey, potId: Uint8Array, stake: bigint, openSecs: bigint, validator: PublicKey, updown = false) {
    const d = this.pdas;
    const pot = d.pot(potId);
    return this.ix(
      [m(payer, true, true), m(d.table(potId), true), m(d.appAuthority()), m(d.config()), m(d.app(), true), m(pot, true),
        m(d.receipt(pot), true), m(this.p.ledger), m(SystemProgram.programId)],
      cat(disc("global", updown ? "create_updown_table" : "create_table"), potId, u64le(stake), i64le(openSecs), validator.toBytes()),
    );
  }

  /** Up/Down, rollup: the seated player bets Up (or Down) for one round. */
  bet(payer: PublicKey, potId: Uint8Array, up: boolean) {
    const d = this.pdas;
    return this.ix(
      [m(payer, true, true), m(d.table(potId), true), m(d.pot(potId)), m(ZEC_USD_FEED)],
      cat(disc("global", "bet"), Uint8Array.of(up ? 1 : 0)),
    );
  }

  /** Up/Down, rollup, anyone: settle the open bet once its round is over. */
  settleBet(potId: Uint8Array) {
    const d = this.pdas;
    return this.ix(
      [m(d.table(potId), true), m(d.pot(potId), true), m(ZEC_USD_FEED), m(d.appAuthority()), m(this.p.ledger)],
      disc("global", "settle_bet"),
    );
  }

  /** One stake, or `rounds` stakes (`join_rounds`): a session of up to that many flips. */
  join(session: PublicKey, potId: Uint8Array, rounds?: number) {
    const d = this.pdas;
    const pot = d.pot(potId);
    const r = new Uint8Array(2);
    new DataView(r.buffer).setUint16(0, rounds ?? 1, true);
    return this.ix(
      [m(session, false, true), m(d.table(potId)), m(d.appAuthority()), m(d.config()), m(d.app(), true),
        m(d.balance(session), true), m(pot, true), m(d.receipt(pot), true), m(this.p.ledger)],
      rounds === undefined ? disc("global", "join") : cat(disc("global", "join_rounds"), r),
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
      [m(payer, true, true), m(d.table(potId), true), m(d.pot(potId)), m(queue, true), m(d.identity()), m(VRF_PROGRAM),
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

  /** `finish`, and the table's return runs `cash_out_table` on Solana (a Magic Action). */
  finishAndPay(payer: PublicKey, potId: Uint8Array) {
    const d = this.pdas;
    return this.ix(
      [m(payer, true, true), m(d.table(potId), true), m(d.pot(potId), true), m(d.appAuthority()), m(this.p.ledger),
        m(d.rentVault()), m(MAGIC_PROGRAM), m(MAGIC_CONTEXT, true)],
      disc("global", "finish_and_pay"),
    );
  }

  /** Rollup, anyone: the rollup closes this table by itself once idle, short or near its deadline. */
  scheduleCloser(payer: PublicKey, potId: Uint8Array, validator: PublicKey) {
    const d = this.pdas;
    return this.ix(
      [m(payer, true, true), m(d.table(potId)), m(d.pot(potId)), m(d.appAuthority()), m(this.p.ledger),
        m(d.rentVault()), m(magicFeeVault(validator)), m(ZEC_USD_FEED), m(MAGIC_PROGRAM), m(MAGIC_CONTEXT, true)],
      disc("global", "schedule_closer"),
    );
  }

  /** Solana, anyone: cash out a returned pot to both seats and close the table (no-op until the pot is back). */
  cashOutTable(potId: Uint8Array, creator: PublicKey, players: PublicKey[]) {
    const d = this.pdas;
    const pot = d.pot(potId);
    return this.ix(
      [m(d.config()), m(d.app()), m(d.receipt(pot), true), m(pot), m(d.table(potId), true), m(creator, true),
        m(d.balance(players[0]), true), m(d.balance(players[1]), true), m(this.p.ledger)],
      disc("global", "cash_out_table"),
    );
  }

  /** The escrow paying the Magic Action's Solana fee: the delegation program's ["balance", rent vault, 255]. */
  actionEscrow() {
    return this.pdas.pda([enc.encode("balance"), this.pdas.rentVault().toBytes(), Uint8Array.of(255)], DLP);
  }

  /** Delegation program `TopUpEphemeralBalance` for the action escrow. */
  topUpActionEscrow(payer: PublicKey, lamports: bigint) {
    return this.ix(
      [m(payer, true, true), m(this.pdas.rentVault()), m(this.actionEscrow(), true), m(SystemProgram.programId)],
      cat(Uint8Array.of(9, 0, 0, 0, 0, 0, 0, 0), u64le(lamports), Uint8Array.of(255)),
      DLP,
    );
  }

  /** Ledger, anyone: refund every buy-in of a pot that never started (after its open deadline). */
  refundPot(potId: Uint8Array, players: PublicKey[]) {
    const d = this.pdas;
    const pot = d.pot(potId);
    return this.ix(
      [m(d.config()), m(d.app()), m(d.receipt(pot), true), m(pot), ...players.map((p) => m(d.balance(p), true))],
      disc("global", "refund_pot"),
      this.p.ledger,
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

  /** Anyone: turns exiting balances (up to 16) into settlement `seq` (the app's `next_settlement`). */
  settle(payer: PublicKey, seq: bigint, sessions: PublicKey[]) {
    const d = this.pdas;
    const settlement = d.pda([enc.encode("settlement"), d.app().toBytes(), u64le(seq)], this.p.ledger);
    return this.ix(
      [m(payer, true, true), m(d.config()), m(d.app(), true), m(settlement, true), m(SystemProgram.programId), ...sessions.map((s) => m(d.balance(s), true))],
      disc("global", "settle"),
      this.p.ledger,
    );
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
  u32() { const v = this.dv.getUint32(this.o, true); this.o += 4; return v; }
  u16() { const v = this.dv.getUint16(this.o, true); this.o += 2; return v; }
  u64() { const v = this.dv.getBigUint64(this.o, true); this.o += 8; return v; }
  i64() { const v = this.dv.getBigInt64(this.o, true); this.o += 8; return v; }
}

function checkDisc(data: Uint8Array, name: string) {
  const d = disc("account", name);
  if (!d.every((x, i) => data[i] === x)) throw new Error(`not a ${name} account`);
}

export const TABLE_STATES = ["Open", "Started", "Flipping", "Landed", "Finished"] as const;
export const POT_STATES = ["Open", "Delegated", "CashedOut", "Refunded"] as const;

export interface TableAcc {
  pot: PublicKey; potId: Uint8Array; stake: bigint; validator: PublicKey; creator: PublicKey; state: (typeof TABLE_STATES)[number]; winner: number;
  /** Flips landed so far, and when the pending one asked the oracle (unix s). */
  flips: number; flipRequestedAt: bigint;
  /** Seated keys and session deadline (set by `start`); winning seats, newest in bit 0. */
  players: PublicKey[]; sessionDeadline: bigint; results: bigint;
  /** 0 coin flip, 1 Up/Down; the open bet; rounds won and Up/Down ties. */
  game: number;
  bet: { open: boolean; up: boolean; seat: number; openPrice: bigint; expiry: number; settlePrice: bigint };
  wins: number; ties: number;
}
export function decodeTable(data: Uint8Array): TableAcc {
  checkDisc(data, "Table");
  const r = new Reader(data);
  const t = { pot: r.key(), potId: r.bytes(32), stake: r.u64(), validator: r.key(), creator: r.key(), state: TABLE_STATES[r.u8()], winner: r.u8() };
  r.bytes(32); // randomness
  r.u8(); // bump
  // Older tables end earlier: before sessions (no flips), or before players (no players).
  const sessions = data.length >= r.o + 12;
  const flips = sessions ? r.u32() : 0;
  const flipRequestedAt = sessions ? r.i64() : 0n;
  const seated = data.length >= r.o + 80;
  const players = seated ? [r.key(), r.key()] : [];
  const sessionDeadline = seated ? r.i64() : 0n;
  const results = seated ? r.u64() : 0n;
  // Up/Down fields: game, the bet slot (3 bytes + 3 × i64), wins, ties.
  const games = data.length >= r.o + 1 + 27 + 8;
  const game = games ? r.u8() : 0;
  const bet = games
    ? { open: r.u8() === 1, up: r.u8() === 1, seat: r.u8(), openPrice: r.i64(), expiry: Number(r.i64()), settlePrice: r.i64() }
    : { open: false, up: false, seat: 0, openPrice: 0n, expiry: 0, settlePrice: 0n };
  return { ...t, flips, flipRequestedAt, players, sessionDeadline, results, game, bet, wins: games ? r.u32() : 0, ties: games ? r.u32() : 0 };
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

export interface AppAcc { state: number; activatesAt: bigint; liability: bigint; feePool: bigint; nextSettlement: bigint; paidCount: bigint; unpaidTotal: bigint }
export function decodeApp(data: Uint8Array): AppAcc {
  checkDisc(data, "App");
  const r = new Reader(data);
  r.key(); r.key(); r.key();
  const state = r.u8();
  const activatesAt = r.i64();
  r.bytes(43); r.bytes(32);
  r.u64(); // deposit_cap
  const liability = r.u64();
  r.u64(); r.u64(); // deposit_count, credited_total
  const feePool = r.u64();
  const nextSettlement = r.u64();
  const paidCount = r.u64();
  const unpaidTotal = r.u64();
  return { state, activatesAt, liability, feePool, nextSettlement, paidCount, unpaidTotal };
}

export interface LedgerParams { entryFeeBps: number; denominations: bigint[]; minExit?: bigint }
/** Ledger Config params the page needs. `params` starts after admin, attestor, 7 guardians
 *  and 4 one-byte fields; `denominations` follows 2 u16, 2 u64, a u8, 5 i64 and a u64. */
export function decodeLedgerParams(data: Uint8Array): LedgerParams {
  checkDisc(data, "Config");
  const p = 8 + 32 + 32 + 7 * 32 + 4;
  const dv = new DataView(data.buffer, data.byteOffset);
  const d0 = p + 2 + 2 + 8 + 8 + 1 + 5 * 8 + 8;
  return { entryFeeBps: dv.getUint16(p, true), denominations: [0, 1, 2, 3].map((i) => dv.getBigUint64(d0 + 8 * i, true)), minExit: dv.getBigUint64(p + 12, true) };
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

/**
 * Signs and sends, then rebroadcasts every 2 s until the transaction is confirmed or its
 * blockhash expires (a node may drop a transaction it was handed once). Preflight is on:
 * a transaction that cannot succeed, e.g. an unfunded fee payer, fails here with its reason
 * instead of silently expiring.
 */
export async function sendTx(conn: Connection, ixs: TransactionInstruction[], feePayer: Keypair, signers: Keypair[] = [], cuLimit?: number) {
  const tx = new Transaction();
  if (cuLimit) tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cuLimit }));
  tx.add(...ixs);
  tx.feePayer = feePayer.publicKey;
  const bh = await conn.getLatestBlockhash("confirmed");
  tx.recentBlockhash = bh.blockhash;
  const all = [feePayer, ...signers.filter((s) => !s.publicKey.equals(feePayer.publicKey))];
  tx.sign(...all);
  return sendSigned(conn, tx.serialize(), bh.lastValidBlockHeight);
}

/** Sends a fully signed transaction and rebroadcasts until confirmed or expired (see sendTx). */
export async function sendSigned(conn: Connection, raw: Uint8Array | Buffer, lastValidBlockHeight: number): Promise<string> {
  let sig: string;
  try {
    sig = await conn.sendRawTransaction(raw, { skipPreflight: false, preflightCommitment: "confirmed", maxRetries: 0 });
  } catch (e: any) {
    const logs: string[] = e?.logs ?? e?.transactionLogs ?? [];
    throw new Error(`${e?.transactionMessage ?? e?.message ?? e}${logs.length ? "\n" + logs.slice(-8).join("\n") : ""}`);
  }
  for (;;) {
    const st = (await conn.getSignatureStatuses([sig])).value[0];
    if (st?.err) {
      const t = await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }).catch(() => null);
      throw new Error(`${JSON.stringify(st.err)}\n${(t?.meta?.logMessages ?? []).slice(-8).join("\n")}`);
    }
    if (st && (st.confirmationStatus === "confirmed" || st.confirmationStatus === "finalized")) return sig;
    if ((await conn.getBlockHeight("confirmed")) > lastValidBlockHeight) throw new Error(`transaction ${sig.slice(0, 16)}… expired before it landed; try again`);
    await new Promise((r) => setTimeout(r, 2000));
    await conn.sendRawTransaction(raw, { skipPreflight: true, maxRetries: 0 }).catch(() => {});
  }
}

// ---------------------------------------------------------------- sealed-bid auctions

export const AUCTION_STATES = ["Registering", "Bidding", "Ended"] as const;
export const AUCTION_SEATS = 9;

export interface AuctionAcc {
  pot: PublicKey; potId: Uint8Array; creator: PublicKey; validator: PublicKey; deposit: bigint;
  registerDeadline: number; commitDeadline: number; revealDeadline: number;
  state: (typeof AUCTION_STATES)[number]; seatCount: number; players: PublicKey[];
  commits: Uint8Array[]; bids: bigint[]; committed: number; revealed: number; winner: number; price: bigint;
}
export function decodeAuction(data: Uint8Array): AuctionAcc {
  checkDisc(data, "Auction");
  const r = new Reader(data);
  const pot = r.key(), potId = r.bytes(32), creator = r.key(), validator = r.key(), deposit = r.u64();
  const registerDeadline = Number(r.i64()), commitDeadline = Number(r.i64()), revealDeadline = Number(r.i64());
  const state = AUCTION_STATES[r.u8()];
  const seatCount = r.u8();
  const players = Array.from({ length: AUCTION_SEATS }, () => r.key());
  const commits = Array.from({ length: AUCTION_SEATS }, () => r.bytes(32));
  const bids = Array.from({ length: AUCTION_SEATS }, () => r.u64());
  const committed = r.u16(), revealed = r.u16(), winner = r.u8(), price = r.u64();
  return { pot, potId, creator, validator, deposit, registerDeadline, commitDeadline, revealDeadline, state, seatCount, players: players.slice(0, seatCount), commits, bids, committed, revealed, winner, price };
}

/** `sha256("ZOL/bid/v1" ‖ auction ‖ bidder ‖ amount (u64 LE) ‖ salt)`, as zol-coinflip checks it. */
export const bidCommitment = (auction: PublicKey, bidder: PublicKey, amount: bigint, salt: Uint8Array) =>
  sha256(cat(enc.encode("ZOL/bid/v1"), auction.toBytes(), bidder.toBytes(), u64le(amount), salt));
/** The salt for a bid: from the session secret and the auction, so nothing needs saving. */
export const bidSalt = (sessionSeed: Uint8Array, auction: PublicKey) =>
  sha256(cat(enc.encode("ZOL/bid-salt/v1"), sessionSeed, auction.toBytes()));

export class Auctions {
  constructor(readonly cf: Coinflip) {}
  private get d() { return this.cf.pdas; }
  private ix(keys: ReturnType<typeof m>[], data: Uint8Array, program = this.cf.p.app) {
    return new TransactionInstruction({ programId: program, keys, data: Buffer.from(data) });
  }
  auction(potId: Uint8Array) { return this.d.pda([enc.encode("auction"), potId], this.cf.p.app); }

  create(payer: PublicKey, seller: PublicKey, potId: Uint8Array, deposit: bigint, secs: [number, number, number], validator: PublicKey) {
    const d = this.d, pot = d.pot(potId);
    return this.ix(
      [m(payer, true, true), m(seller, false, true), m(this.auction(potId), true), m(d.appAuthority()), m(d.config()), m(d.app(), true),
        m(pot, true), m(d.receipt(pot), true), m(d.balance(seller), true), m(this.cf.p.ledger), m(SystemProgram.programId)],
      cat(disc("global", "create_auction"), potId, u64le(deposit), ...secs.map((s) => i64le(BigInt(s))), validator.toBytes()),
    );
  }
  register(session: PublicKey, potId: Uint8Array) {
    const d = this.d, pot = d.pot(potId);
    return this.ix(
      [m(session, false, true), m(this.auction(potId), true), m(d.appAuthority()), m(d.config()), m(d.app(), true),
        m(d.balance(session), true), m(pot, true), m(d.receipt(pot), true), m(this.cf.p.ledger)],
      disc("global", "register_bidder"),
    );
  }
  start(payer: PublicKey, potId: Uint8Array) {
    const d = this.d, pot = d.pot(potId), a = this.auction(potId);
    return this.ix(
      [m(payer, true, true), m(d.buffer(a, this.cf.p.app), true), m(d.dlp("delegation", a), true), m(d.dlp("delegation-metadata", a), true),
        m(a, true), m(d.appAuthority()), m(d.config()), m(d.app(), true), m(d.receipt(pot), true), m(pot, true),
        m(d.buffer(pot, this.cf.p.ledger), true), m(d.dlp("delegation", pot), true), m(d.dlp("delegation-metadata", pot), true),
        m(this.cf.p.ledger), m(this.cf.p.app), m(DLP), m(SystemProgram.programId)],
      disc("global", "start_auction"),
    );
  }
  commit(bidder: PublicKey, potId: Uint8Array, commitment: Uint8Array) {
    return this.ix([m(bidder, false, true), m(this.auction(potId), true)], cat(disc("global", "commit_bid"), commitment));
  }
  reveal(bidder: PublicKey, potId: Uint8Array, amount: bigint, salt: Uint8Array) {
    return this.ix([m(bidder, false, true), m(this.auction(potId), true)], cat(disc("global", "reveal_bid"), u64le(amount), salt));
  }
  end(potId: Uint8Array, validator: PublicKey) {
    const d = this.d;
    return this.ix(
      [m(this.auction(potId), true), m(d.pot(potId), true), m(d.appAuthority()), m(this.cf.p.ledger), m(d.rentVault()),
        m(magicFeeVault(validator), true), m(MAGIC_PROGRAM), m(MAGIC_CONTEXT, true)],
      disc("global", "end_auction"),
    );
  }
  scheduleEnd(payer: PublicKey, potId: Uint8Array, validator: PublicKey) {
    const d = this.d;
    return this.ix(
      [m(payer, true, true), m(this.auction(potId)), m(d.pot(potId)), m(d.appAuthority()), m(this.cf.p.ledger), m(d.rentVault()),
        m(magicFeeVault(validator)), m(MAGIC_PROGRAM), m(MAGIC_CONTEXT, true)],
      disc("global", "schedule_auction_end"),
    );
  }
  cashOut(potId: Uint8Array, players: PublicKey[]) {
    const d = this.d, pot = d.pot(potId);
    return this.ix(
      [m(d.config()), m(d.app()), m(d.receipt(pot), true), m(pot), m(this.cf.p.ledger), ...players.map((p) => m(d.balance(p), true))],
      disc("global", "cash_out_auction"),
    );
  }
}
