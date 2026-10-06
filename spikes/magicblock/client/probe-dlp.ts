// Probe whether a cluster's deployed delegation program knows RequestUndelegation (26) and
// UndelegateWithRollbackAfterTimeout (27), by simulation only (sigVerify off, no SOL needed).
// Usage: bun client/probe-dlp.ts <rpc-url> <funded-fee-payer-pubkey>
import { Connection, Keypair, PublicKey, TransactionInstruction, TransactionMessage, VersionedTransaction, SystemProgram } from "@solana/web3.js";
import { DELEGATION_PROGRAM_ID } from "@magicblock-labs/ephemeral-rollups-sdk";

const [url, feePayerStr] = process.argv.slice(2);
const conn = new Connection(url, "confirmed");
const feePayer = new PublicKey(feePayerStr);
const dummy = () => Keypair.generate().publicKey;

async function sim(disc: number, nAccounts: number) {
  const keys = [{ pubkey: feePayer, isSigner: true, isWritable: true }];
  for (let i = 1; i < nAccounts; i++) keys.push({ pubkey: i === nAccounts - 1 ? SystemProgram.programId : dummy(), isSigner: false, isWritable: i % 2 === 1 });
  const data = Buffer.alloc(8); data.writeBigUInt64LE(BigInt(disc));
  const ix = new TransactionInstruction({ programId: DELEGATION_PROGRAM_ID, keys, data });
  const { blockhash } = await conn.getLatestBlockhash();
  const msg = new TransactionMessage({ payerKey: feePayer, recentBlockhash: blockhash, instructions: [ix] }).compileToV0Message();
  const r = await conn.simulateTransaction(new VersionedTransaction(msg), { sigVerify: false, replaceRecentBlockhash: true });
  return { disc, err: r.value.err, logs: (r.value.logs ?? []).filter((l) => !/consumed|invoke \[/.test(l)) };
}

for (const [d, n] of [[26, 7], [27, 9], [99, 7], [3, 12]] as const) console.log(JSON.stringify({ url, ...(await sim(d, n)) }));
