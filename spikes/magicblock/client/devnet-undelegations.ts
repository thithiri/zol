// Passive devnet measurement of commit_and_undelegate latency (no SOL needed):
// find recent DLP Undelegate transactions on devnet base, then find the ER transaction that
// scheduled the commit+undelegate for that account, and diff their block times.
// Usage: bun client/devnet-undelegations.ts [pages] [max]
import { Connection, PublicKey } from "@solana/web3.js";

const base = new Connection(process.env.BASE_URL ?? "https://api.devnet.solana.com", "confirmed");
const DLP = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const ERS: Record<string, string> = {
  MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57: "https://devnet-as.magicblock.app",
  MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e: "https://devnet-eu.magicblock.app",
  MUS3hc9TCw4cGC12vHNoYcCGzJG1txjgQLZWVoeNHNd: "https://devnet-us.magicblock.app",
};
const PAGES = Number(process.argv[2] ?? 2), MAX = Number(process.argv[3] ?? 30);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function getTx(conn: Connection, sig: string) {
  for (let i = 0; i < 5; i++) {
    try { return await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }); }
    catch { await sleep(800 * (i + 1)); }
  }
  return null;
}

const sigs: any[] = []; let before: string | undefined;
for (let p = 0; p < PAGES; p++) {
  const page = await base.getSignaturesForAddress(DLP, { limit: 1000, before });
  if (!page.length) break; sigs.push(...page); before = page[page.length - 1].signature;
}
console.error(`scanning ${sigs.length} devnet DLP signatures`);
let n = 0;
for (const s of sigs) {
  if (s.err) continue;
  const t = await getTx(base, s.signature);
  if (!t) continue;
  const msg = t.transaction.message;
  const keys = msg.getAccountKeys({ accountKeysFromLookups: t.meta?.loadedAddresses }).staticAccountKeys.concat(
    t.meta?.loadedAddresses?.writable ?? [], t.meta?.loadedAddresses?.readonly ?? []);
  const undel = msg.compiledInstructions.find((ix) => keys[ix.programIdIndex].equals(DLP) && ix.data.length >= 8 && Buffer.from(ix.data).readBigUInt64LE(0) === 3n);
  if (!undel) continue;
  const validator = keys[undel.accountKeyIndexes[0]].toBase58();
  const delegated = keys[undel.accountKeyIndexes[1]];
  const erUrl = ERS[validator];
  const rec: any = { base_sig: s.signature, base_slot: t.slot, base_time: t.blockTime, validator, delegated: delegated.toBase58(), er: erUrl ?? "unknown" };
  if (erUrl) {
    const er = new Connection(erUrl, "confirmed");
    const erSigs = await er.getSignaturesForAddress(delegated, { limit: 25 }).catch(() => []);
    for (const es of erSigs) {
      if (es.err || (es.blockTime ?? 0) > (t.blockTime ?? 0) + 2) continue;
      const et = await getTx(er, es.signature);
      const logs = et?.meta?.logMessages ?? [];
      if (logs.some((l) => l.includes("ScheduledCommitSent signature: ")) && logs.some((l) => /undelegat/i.test(l))) {
        rec.er_sig = es.signature; rec.er_time = et?.blockTime; rec.delay_s = (t.blockTime ?? 0) - (et?.blockTime ?? 0);
        break;
      }
    }
  }
  console.log(JSON.stringify(rec));
  if (++n >= MAX) break;
  await sleep(200);
}
