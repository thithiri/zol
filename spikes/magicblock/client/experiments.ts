// Targeted experiments: (a) SOL-less fee payer in the ER, writes to non-delegated accounts,
// (e) fees charged to the delegation rent payer, the 10-commit limit.
// Usage: bun client/experiments.ts
import { Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram } from "@solana/web3.js";
import {
  base, er, payer, nosol, PROGRAM_ID, potPda, decodePot, fmtPot, configPda,
  ixInitPot, ixDelegatePot, ixMoveSeat, ixCommitPot, ixClosePot, ixSetConfig,
  send, getLogs, pollAccount, validatorIdentity, log, now, sleep,
} from "./common.ts";
import { delegationRecordPdaFromDelegatedAccount, delegationMetadataPdaFromDelegatedAccount } from "@magicblock-labs/ephemeral-rollups-sdk";

const OUT = "experiments.jsonl";
const short = (e: any) => String(e?.message ?? e).split("\n").slice(0, 1).join(" ").slice(0, 300);
const logsOf = (e: any) => (e?.logs ?? []).filter((l: string) => !/consumed|invoke \[|success$/.test(l)).slice(-6);

async function newDelegatedPot(validator: PublicKey, rentPayer: Keypair = payer) {
  const id = BigInt(Date.now()) * 1000n + BigInt(Math.floor(Math.random() * 1000));
  await send(base, [ixInitPot(id, [100, 100, 100, 100])], [payer]);
  await send(base, [ixDelegatePot(id, validator, rentPayer.publicKey)], [payer, rentPayer]);
  await pollAccount(er, potPda(id), (a) => a && a.owner.equals(PROGRAM_ID), now(), 30000);
  return id;
}

async function main() {
  const validator = await validatorIdentity();
  log(OUT, { event: "start", validator: validator.toBase58(), nosol: nosol.publicKey.toBase58() });
  log(OUT, { event: "nosol_balances", base: await base.getBalance(nosol.publicKey), er: await er.getBalance(nosol.publicKey) });

  // (a) SOL-less key as fee payer + signer for an ER tx touching a delegated account.
  {
    const id = await newDelegatedPot(validator);
    const r: Record<string, unknown> = { event: "a_nosol_fee_payer", pot: potPda(id).toBase58() };
    try { const s = await send(er, [ixMoveSeat(id, 0, 1, 7, nosol.publicKey)], [nosol]); r.move_seat = "OK " + s.sig; r.ms = Math.round(s.ms); }
    catch (e) { r.move_seat = short(e); r.move_logs = logsOf(e); }
    try { const s = await send(er, [ixMoveSeat(id, 0, 1, 7, nosol.publicKey)], [nosol], { skipPreflight: false }); r.move_seat_preflight = "OK " + s.sig; }
    catch (e) { r.move_seat_preflight = short(e); }
    // nosol as payer of a commit (magic program requires payer signer+writable).
    try { const s = await send(er, [ixCommitPot(id, nosol.publicKey)], [nosol]); r.commit = "OK " + s.sig; }
    catch (e) { r.commit = short(e); r.commit_logs = logsOf(e); }
    try { const s = await send(er, [ixClosePot(id, nosol.publicKey)], [nosol]); r.close = "OK " + s.sig;
      r.returned_ms = Math.round(await pollAccount(base, potPda(id), (a) => a && a.owner.equals(PROGRAM_ID), now(), 60000)); }
    catch (e) { r.close = short(e); r.close_logs = logsOf(e); }
    r.nosol_er_balance_after = await er.getBalance(nosol.publicKey);
    r.nosol_er_account_after = JSON.stringify(await er.getAccountInfo(nosol.publicKey));
    r.er_state = fmtPot(decodePot((await er.getAccountInfo(potPda(id)))!.data));
    log(OUT, r);
    // If close failed, clean up with funded payer.
    if (!String(r.close).startsWith("OK")) await send(er, [ixClosePot(id)], [payer]).catch(() => {});
  }

  // (b) writing a NON-delegated account in the ER must fail; reading it works (covered in run.ts).
  {
    const r: Record<string, unknown> = { event: "b_write_non_delegated_in_er" };
    try { await send(er, [ixSetConfig(777)], [payer]); r.set_config_in_er = "UNEXPECTED_OK"; }
    catch (e) { r.set_config_in_er = short(e); }
    const a = await er.getAccountInfo(configPda());
    r.config_in_er_owner = a?.owner.toBase58(); r.config_in_er_value = a ? a.data.readBigUInt64LE(8).toString() : null;
    r.config_on_base_value = (await base.getAccountInfo(configPda()))!.data.readBigUInt64LE(8).toString();
    log(OUT, r);
  }

  // (e) fees: a dedicated rent payer, delegate -> k commits -> undelegate; net lamport change.
  const rp = Keypair.generate();
  await send(base, [SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: rp.publicKey, lamports: LAMPORTS_PER_SOL })], [payer]);
  for (const k of [0, 1, 3]) {
    const b0 = await base.getBalance(rp.publicKey);
    const id = await newDelegatedPot(validator, rp);
    const pot = potPda(id);
    const recL = await base.getBalance(delegationRecordPdaFromDelegatedAccount(pot));
    const metaL = await base.getBalance(delegationMetadataPdaFromDelegatedAccount(pot));
    const b1 = await base.getBalance(rp.publicKey);
    for (let i = 0; i < k; i++) {
      await send(er, [ixMoveSeat(id, 0, 1, 1, payer.publicKey)], [payer]);
      const want = decodePot((await er.getAccountInfo(pot))!.data).moves;
      await send(er, [ixCommitPot(id)], [payer]);
      await pollAccount(base, pot, (a) => a && decodePot(a.data).moves === want, now(), 60000);
    }
    await send(er, [ixClosePot(id)], [payer]);
    await pollAccount(base, pot, (a) => a && a.owner.equals(PROGRAM_ID), now(), 60000);
    await sleep(1500);
    const b2 = await base.getBalance(rp.publicKey);
    log(OUT, { event: "e_fees", commits_before_close: k, rent_payer_locked: b0 - b1, record_lamports: recL, metadata_lamports: metaL,
      rent_payer_net_after_undelegate: b2 - b0, fee_charged_lamports: b0 - b2 });
  }

  // (e) 10-commit limit for plain commits with a non-delegated payer.
  {
    const id = await newDelegatedPot(validator);
    const r: Record<string, unknown> = { event: "e_commit_limit" };
    const results: string[] = [];
    for (let i = 1; i <= 12; i++) {
      await send(er, [ixMoveSeat(id, 0, 1, 1, payer.publicKey)], [payer]);
      try { await send(er, [ixCommitPot(id)], [payer]); results.push(`${i}:ok`); }
      catch (e) { results.push(`${i}:FAIL ${logsOf(e).join(" / ").slice(0, 300)}`); }
      await sleep(1200); // let each commit land so nonces advance
    }
    r.commits = results;
    try { await send(er, [ixClosePot(id)], [payer]); r.close_after_limit = "OK";
      r.returned_ms = Math.round(await pollAccount(base, potPda(id), (a) => a && a.owner.equals(PROGRAM_ID), now(), 60000)); }
    catch (e) { r.close_after_limit = short(e); }
    log(OUT, r);
  }
}
main().catch((e) => { console.error(e); process.exit(1); });
