// Devnet ER probes that need no SOL: fee payer with zero balance, reading undelegated accounts.
// Usage: bun client/probe-devnet-er.ts [er-url]
import { ComputeBudgetProgram, Connection, Keypair, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { readFileSync } from "node:fs";

const ER = process.argv[2] ?? "https://devnet-as.magicblock.app";
const er = new Connection(ER, "confirmed");
const base = new Connection("https://api.devnet.solana.com", "confirmed");
const nosol = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(new URL("../keys/nosol.json", import.meta.url), "utf8"))));
const MEMO = new PublicKey("MemoSq4gqABAXKb96qnH28ysdcEjhR2Kq4DjQd5yHg5"); // SPL memo v2
const out = (o: object) => console.log(JSON.stringify({ ts: new Date().toISOString(), er: ER, ...o }));

const rpc = async (method: string, params: unknown[] = []) =>
  (await (await fetch(ER, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) })).json());

out({ getIdentity: await rpc("getIdentity"), version: await rpc("getVersion") });
out({ nosol: nosol.publicKey.toBase58(), base_balance: await base.getBalance(nosol.publicKey), er_balance: await er.getBalance(nosol.publicKey).catch((e) => String(e)) });

// Undelegated base accounts as seen through the ER (cloned on demand?).
for (const k of ["GfRxw3VZatdKDGkpnxZoASvxEGhK9zJ3iLd9KFWaDtR2", "SysvarC1ock11111111111111111111111111111111"]) {
  const t0 = performance.now();
  const a = await er.getAccountInfo(new PublicKey(k)).catch((e) => String(e));
  const b = await base.getAccountInfo(new PublicKey(k));
  out({ account: k, er_ms: Math.round(performance.now() - t0), er: a && typeof a === "object" ? { owner: a.owner.toBase58(), lamports: a.lamports, len: a.data.length } : a, base: b ? { owner: b.owner.toBase58(), lamports: b.lamports, len: b.data.length } : null });
}

// Zero-SOL fee payer: memo-only tx (no delegated accounts) and compute-budget-only tx.
for (const [name, ixs] of [
  ["memo", [new TransactionInstruction({ programId: MEMO, keys: [{ pubkey: nosol.publicKey, isSigner: true, isWritable: false }], data: Buffer.from("zol spike 0.9 " + Date.now()) })]],
  ["compute_budget_only", [ComputeBudgetProgram.setComputeUnitLimit({ units: 1000 + Math.floor(Math.random() * 1000) })]],
] as const) {
  const tx = new Transaction().add(...ixs);
  tx.feePayer = nosol.publicKey;
  tx.recentBlockhash = (await er.getLatestBlockhash()).blockhash;
  tx.sign(nosol);
  for (const skipPreflight of [false, true]) {
    try {
      const sig = await er.sendRawTransaction(tx.serialize(), { skipPreflight });
      const st = await er.confirmTransaction(sig, "confirmed").catch((e) => ({ value: { err: String(e) } }));
      const t = await er.getTransaction(sig, { maxSupportedTransactionVersion: 0 }).catch(() => null);
      out({ zero_sol_fee_payer: name, skipPreflight, sig, err: st.value.err, fee: t?.meta?.fee, logs: t?.meta?.logMessages });
      break;
    } catch (e: any) { out({ zero_sol_fee_payer: name, skipPreflight, error: String(e.message ?? e).slice(0, 400) }); }
  }
}
