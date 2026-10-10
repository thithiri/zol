// The local stack's dumped VRF queues (@magicblock-labs/ephemeral-validator 0.14.10) arrive
// full of stale requests from another chain. Their request slots are far ahead of the local
// rollup, so the oracle can never fulfil them (OracleMustProvideInDifferentSlot), and new
// requests fail with AccountDataTooSmall. The VRF program's permissionless
// PurgeExpiredRequests (ix 9) clears requests older than its TTL.
//   npx vite-node scripts/purge-vrf-queue.ts [rpc] [queue index]
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { Connection, Keypair, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";

const rpc = process.argv[2] ?? "http://127.0.0.1:27799";
const index = Number(process.argv[3] ?? 1);
const VRF = new PublicKey("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz");
const ORACLE = new PublicKey("paywJiVATrVDLYLmowJqzG6MsaCt77L8WyTnBb2754t"); // local oracle identity
const queue = PublicKey.findProgramAddressSync([Buffer.from("queue"), ORACLE.toBuffer(), Buffer.from([index])], VRF)[0];
const conn = new Connection(rpc, "confirmed");
const payer = Keypair.generate(); // rollup transactions are free
const before = (await conn.getAccountInfo(queue))!.data;
const tx = new Transaction().add(new TransactionInstruction({
  programId: VRF,
  keys: [{ pubkey: ORACLE, isSigner: false, isWritable: false }, { pubkey: queue, isSigner: false, isWritable: true }],
  data: Buffer.from([9, 0, 0, 0, 0, 0, 0, 0, index]), // 8-byte discriminator, then the index
}));
tx.feePayer = payer.publicKey;
tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
tx.sign(payer);
const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true });
await conn.confirmTransaction(sig, "confirmed");
const t = await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0 });
const after = (await conn.getAccountInfo(queue))!.data;
console.log(JSON.stringify({ queue: queue.toBase58(), err: t?.meta?.err, logs: t?.meta?.logMessages, items_before: before.readUInt32LE(8), items_after: after.readUInt32LE(8) }));
