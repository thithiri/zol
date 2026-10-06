// Shared client helpers for the ZOL MagicBlock spike (web3.js v1, no Anchor IDL).
import {
  ComputeBudgetProgram, Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction,
  type Commitment,
} from "@solana/web3.js";
import {
  DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID,
  delegationRecordPdaFromDelegatedAccount, delegationMetadataPdaFromDelegatedAccount,
  delegateBufferPdaFromDelegatedAccountAndOwnerProgram,
  commitStatePdaFromDelegatedAccount, commitRecordPdaFromDelegatedAccount,
} from "@magicblock-labs/ephemeral-rollups-sdk";
import { createHash } from "node:crypto";
import { readFileSync, appendFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
export const NET = process.env.NET ?? "local";
export const BASE_URL = process.env.BASE_URL ?? (NET === "devnet" ? "https://api.devnet.solana.com" : "http://127.0.0.1:28899");
export const ER_URL = process.env.ER_URL ?? (NET === "devnet" ? "https://devnet-as.magicblock.app" : "http://127.0.0.1:27799");

export const base = new Connection(BASE_URL, "confirmed");
export const er = new Connection(ER_URL, "confirmed");

export function kp(name: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(join(ROOT, "keys", name + ".json"), "utf8"))));
}
export const payer = kp("payer");
export const nosol = kp("nosol");
export const PROGRAM_ID = kp("program").publicKey;
export { DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID };

export const now = () => performance.now();
export const iso = () => new Date().toISOString();
export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export function log(file: string, obj: Record<string, unknown>) {
  const line = JSON.stringify({ ts: iso(), net: NET, ...obj });
  console.log(line);
  appendFileSync(join(ROOT, "logs", file), line + "\n");
}

function disc(name: string): Buffer {
  return createHash("sha256").update("global:" + name).digest().subarray(0, 8);
}
const u64 = (n: bigint | number) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
const u8 = (n: number) => Buffer.from([n]);
const vecU64 = (xs: (bigint | number)[]) => { const b = Buffer.alloc(4); b.writeUInt32LE(xs.length); return Buffer.concat([b, ...xs.map(u64)]); };

export const configPda = () => PublicKey.findProgramAddressSync([Buffer.from("config")], PROGRAM_ID)[0];
export const potPda = (id: bigint) => PublicKey.findProgramAddressSync([Buffer.from("pot"), u64(id)], PROGRAM_ID)[0];
export const undelRequestPda = (acc: PublicKey) =>
  PublicKey.findProgramAddressSync([Buffer.from("undelegation-request"), acc.toBuffer()], DELEGATION_PROGRAM_ID)[0];

const m = (pubkey: PublicKey, isWritable = false, isSigner = false) => ({ pubkey, isWritable, isSigner });
const ix = (keys: ReturnType<typeof m>[], data: Buffer) => new TransactionInstruction({ programId: PROGRAM_ID, keys, data });

export const ixInitConfig = (value: number) =>
  ix([m(configPda(), true), m(payer.publicKey, true, true), m(SystemProgram.programId)], Buffer.concat([disc("init_config"), u64(value)]));
export const ixSetConfig = (value: number) =>
  ix([m(configPda(), true), m(payer.publicKey, false, true)], Buffer.concat([disc("set_config"), u64(value)]));
export const ixInitPot = (id: bigint, amounts: number[]) =>
  ix([m(potPda(id), true), m(payer.publicKey, true, true), m(SystemProgram.programId)], Buffer.concat([disc("init_pot"), u64(id), vecU64(amounts)]));
export function ixDelegatePot(id: bigint, validator?: PublicKey, rentPayer: PublicKey = payer.publicKey) {
  const pot = potPda(id);
  const keys = [
    m(rentPayer, true, true),
    m(delegateBufferPdaFromDelegatedAccountAndOwnerProgram(pot, PROGRAM_ID), true),
    m(delegationRecordPdaFromDelegatedAccount(pot), true),
    m(delegationMetadataPdaFromDelegatedAccount(pot), true),
    m(pot, true),
    m(PROGRAM_ID),
    m(DELEGATION_PROGRAM_ID),
    m(SystemProgram.programId),
  ];
  if (validator) keys.push(m(validator));
  return ix(keys, Buffer.concat([disc("delegate_pot"), u64(id)]));
}
export const ixMoveSeat = (id: bigint, from: number, to: number, amount: number, authority: PublicKey) =>
  ix([m(potPda(id), true), m(configPda()), m(authority, false, true)], Buffer.concat([disc("move_seat"), u8(from), u8(to), u64(amount)]));
const commitKeys = (id: bigint, p: PublicKey) => [m(p, true, true), m(potPda(id), true), m(MAGIC_PROGRAM_ID), m(MAGIC_CONTEXT_ID, true)];
export const ixCommitPot = (id: bigint, p: PublicKey = payer.publicKey) => ix(commitKeys(id, p), disc("commit_pot"));
export const ixClosePot = (id: bigint, p: PublicKey = payer.publicKey) => ix(commitKeys(id, p), disc("close_pot"));
export const ixCashOutCheck = (id: bigint) => ix([m(potPda(id))], Buffer.concat([disc("cash_out_check"), u64(id)]));
export function ixRequestUndel(id: bigint, rentPayer: PublicKey = payer.publicKey) {
  const pot = potPda(id);
  return ix([
    m(rentPayer, true, true), m(pot), m(PROGRAM_ID), m(undelRequestPda(pot), true),
    m(delegationRecordPdaFromDelegatedAccount(pot)), m(delegationMetadataPdaFromDelegatedAccount(pot), true),
    m(DELEGATION_PROGRAM_ID), m(SystemProgram.programId),
  ], Buffer.concat([disc("request_pot_undelegation"), u64(id)]));
}
export function ixRollback(id: bigint, validator: PublicKey, rentPayer: PublicKey = payer.publicKey) {
  const pot = potPda(id);
  return ix([
    m(pot, true), m(PROGRAM_ID), m(undelRequestPda(pot), true),
    m(delegationRecordPdaFromDelegatedAccount(pot), true), m(delegationMetadataPdaFromDelegatedAccount(pot), true),
    m(rentPayer, true), m(commitStatePdaFromDelegatedAccount(pot), true), m(commitRecordPdaFromDelegatedAccount(pot), true),
    m(validator, true), m(DELEGATION_PROGRAM_ID),
  ], Buffer.concat([disc("rollback_pot"), u64(id)]));
}

export interface PotState { potId: bigint; seats: number; amounts: bigint[]; moves: bigint; lastConfigSeen: bigint }
export function decodePot(data: Buffer): PotState {
  let o = 8;
  const potId = data.readBigUInt64LE(o); o += 8;
  const seats = data[o]; o += 1;
  const amounts: bigint[] = [];
  for (let i = 0; i < 8; i++) { amounts.push(data.readBigUInt64LE(o)); o += 8; }
  const moves = data.readBigUInt64LE(o); o += 8;
  const lastConfigSeen = data.readBigUInt64LE(o);
  return { potId, seats, amounts: amounts.slice(0, seats), moves, lastConfigSeen };
}
export const fmtPot = (p: PotState) => `seats=${p.seats} amounts=[${p.amounts.join(",")}] moves=${p.moves} cfg=${p.lastConfigSeen}`;

let uniq_ctr = Math.floor(Math.random() * 1000);
export async function send(conn: Connection, ixs: TransactionInstruction[], signers: Keypair[], opts: { feePayer?: Keypair; skipPreflight?: boolean; commitment?: Commitment } = {}) {
  const feePayer = opts.feePayer ?? signers[0];
  // unique CU limit so identical ixs with the same blockhash are distinct txs
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 200_000 + (uniq_ctr++ % 100_000) }), ...ixs);
  tx.feePayer = feePayer.publicKey;
  const bh = await conn.getLatestBlockhash();
  tx.recentBlockhash = bh.blockhash;
  const signersU = [feePayer, ...signers.filter((s) => !s.publicKey.equals(feePayer.publicKey))];
  tx.sign(...signersU);
  const t0 = now();
  const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: opts.skipPreflight ?? true });
  const res = await conn.confirmTransaction({ signature: sig, ...bh }, opts.commitment ?? "confirmed");
  const ms = now() - t0;
  if (res.value.err) {
    const t = await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }).catch(() => null);
    const err = new Error(`tx ${sig} failed: ${JSON.stringify(res.value.err)}\n${(t?.meta?.logMessages ?? []).join("\n")}`);
    (err as any).sig = sig; (err as any).logs = t?.meta?.logMessages;
    throw err;
  }
  return { sig, ms };
}

export async function getLogs(conn: Connection, sig: string): Promise<string[]> {
  for (let i = 0; i < 20; i++) {
    const t = await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }).catch(() => null);
    if (t?.meta) return t.meta.logMessages ?? [];
    await sleep(200);
  }
  return [];
}

/** Poll until pred(accountInfo) holds; returns elapsed ms (from t0) or -1 on timeout. */
export async function pollAccount(conn: Connection, key: PublicKey, pred: (a: any) => boolean, t0: number, timeoutMs: number, commitment: Commitment = "confirmed", intervalMs = 100) {
  while (now() - t0 < timeoutMs) {
    const a = await conn.getAccountInfo(key, commitment).catch(() => null);
    if (pred(a)) return now() - t0;
    await sleep(intervalMs);
  }
  return -1;
}

export async function validatorIdentity(): Promise<PublicKey> {
  const r = await fetch(ER_URL, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }) });
  const j: any = await r.json();
  return new PublicKey(j.result.identity);
}
