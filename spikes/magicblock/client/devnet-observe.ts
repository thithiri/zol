// Passive devnet measurement (no SOL needed): find recent commit / commit+undelegate intents
// scheduled on a devnet ER, follow them to the base-layer transaction, and compute the delay.
// Usage: bun client/devnet-observe.ts [er-url] [limit]
import { Connection, PublicKey } from "@solana/web3.js";

const ER = process.argv[2] ?? "https://devnet-as.magicblock.app";
const LIMIT = Number(process.argv[3] ?? 200);
const er = new Connection(ER, "confirmed");
const base = new Connection(process.env.BASE_URL ?? "https://api.devnet.solana.com", "confirmed");
const MAGIC_CONTEXT = new PublicKey("MagicContext1111111111111111111111111111111");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const pick = (logs: string[], prefix: string) => logs.find((l) => l.includes(prefix))?.split(prefix)[1]?.trim();

async function getTx(conn: Connection, sig: string) {
  for (let i = 0; i < 4; i++) {
    try { return await conn.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }); }
    catch { await sleep(500 * (i + 1)); }
  }
  return null;
}

const PAGES = Number(process.argv[4] ?? 1);
const sigs: Awaited<ReturnType<typeof er.getSignaturesForAddress>> = [];
let before: string | undefined;
for (let p = 0; p < PAGES; p++) {
  const page = await er.getSignaturesForAddress(MAGIC_CONTEXT, { limit: LIMIT, before });
  if (!page.length) break;
  sigs.push(...page); before = page[page.length - 1].signature;
}
console.error(`scanning ${sigs.length} ER signatures`);
let n = 0;
for (const s of sigs) {
  if (s.err) continue;
  const t = await getTx(er, s.signature);
  const logs = t?.meta?.logMessages ?? [];
  const sentSig = pick(logs, "ScheduledCommitSent signature: ");
  if (!sentSig) continue;
  const undelegate = logs.some((l) => /undelegat/i.test(l));
  const sent = await getTx(er, sentSig);
  const baseSig = pick(sent?.meta?.logMessages ?? [], "ScheduledCommitSent signature[0]: ");
  if (!baseSig) { console.log(JSON.stringify({ er_sig: s.signature, er_time: t?.blockTime, sentSig, note: "no base sig (pending or failed)" })); continue; }
  const b = await getTx(base, baseSig);
  const baseLogs = b?.meta?.logMessages ?? [];
  console.log(JSON.stringify({
    er_sig: s.signature, er_slot: t?.slot, er_time: t?.blockTime, undelegate,
    sent_er_time: sent?.blockTime, base_sig: baseSig, base_slot: b?.slot, base_time: b?.blockTime, base_err: b?.meta?.err ?? null,
    base_has_undelegate: baseLogs.some((l) => /ProcessUndelegation|Undelegat/i.test(l)),
    delay_s: b?.blockTime && t?.blockTime ? b.blockTime - t.blockTime : null,
  }));
  if (++n >= Number(process.env.MAX ?? 60)) break;
  await sleep(150);
}
