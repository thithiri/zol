// End-to-end pot lifecycle with timings. Usage: bun client/run.ts [runs]
import { PublicKey } from "@solana/web3.js";
import {
  base, er, payer, nosol, PROGRAM_ID, DELEGATION_PROGRAM_ID, configPda, potPda, decodePot, fmtPot,
  ixInitConfig, ixSetConfig, ixInitPot, ixDelegatePot, ixMoveSeat, ixCommitPot, ixClosePot, ixCashOutCheck,
  send, getLogs, pollAccount, validatorIdentity, log, now, sleep,
} from "./common.ts";
import { GetCommitmentSignature } from "@magicblock-labs/ephemeral-rollups-sdk";

const RUNS = Number(process.argv[2] ?? 5);
const OUT = "runs.jsonl";

async function baseTxInfo(sig: string) {
  const t = await base.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }).catch(() => null);
  return t ? { slot: t.slot, blockTime: t.blockTime } : null;
}

async function main() {
  const validator = await validatorIdentity();
  log(OUT, { event: "start", validator: validator.toBase58(), program: PROGRAM_ID.toBase58(), payer: payer.publicKey.toBase58() });

  if (!(await base.getAccountInfo(configPda()))) await send(base, [ixInitConfig(1000)], [payer]);

  for (let r = 0; r < RUNS; r++) {
    const id = BigInt(Date.now()) * 1000n + BigInt(r);
    const pot = potPda(id);
    const rec: Record<string, unknown> = { event: "run", run: r, potId: id.toString(), pot: pot.toBase58() };

    await send(base, [ixInitPot(id, [100, 100, 100, 100])], [payer]);

    // Delegate on base; wait for the ER to see it.
    let t0 = now();
    const del = await send(base, [ixDelegatePot(id, validator)], [payer]);
    rec.delegate_base_confirm_ms = Math.round(del.ms);
    const baseOwnerDuring = (await base.getAccountInfo(pot))!;
    rec.owner_while_delegated = baseOwnerDuring.owner.toBase58();
    rec.base_data_while_delegated = fmtPot(decodePot(baseOwnerDuring.data));
    rec.er_visible_ms = Math.round(await pollAccount(er, pot, (a) => a && a.owner.equals(PROGRAM_ID), t0, 30000));

    // cash_out_check while delegated: must fail with NotReturned.
    try { await send(base, [ixCashOutCheck(id)], [payer]); rec.cashout_while_delegated = "UNEXPECTED_OK"; }
    catch (e: any) { rec.cashout_while_delegated = (e.logs ?? []).filter((l: string) => /owner|NotReturned|Error Code/.test(l)).join(" | "); }

    // Mutations in the ER.
    const mv: number[] = [];
    for (let i = 0; i < 5; i++) mv.push(Math.round((await send(er, [ixMoveSeat(id, 0, 1, 10, payer.publicKey)], [payer])).ms));
    rec.er_move_ms = mv;

    // Config read in ER: change Config on base, then see when the ER observes it.
    const cfgVal = 2000 + r;
    const tc = now();
    await send(base, [ixSetConfig(cfgVal)], [payer]);
    rec.set_config_base_ms = Math.round(now() - tc);
    let seen = -1, tries = 0;
    while (now() - tc < 30000) {
      tries++;
      await send(er, [ixMoveSeat(id, 1, 2, 1, payer.publicKey)], [payer]);
      const a = await er.getAccountInfo(pot);
      if (a && decodePot(a.data).lastConfigSeen === BigInt(cfgVal)) { seen = Math.round(now() - tc); break; }
      await sleep(100);
    }
    rec.config_update_visible_in_er_ms = seen; rec.config_probe_tx = tries;

    const erState = decodePot((await er.getAccountInfo(pot))!.data);
    rec.er_state = fmtPot(erState);

    // Commit (stay delegated). Commit latency = ER commit tx sent -> base copy equals ER state.
    t0 = now();
    const c = await send(er, [ixCommitPot(id)], [payer]);
    rec.commit_er_confirm_ms = Math.round(c.ms);
    const sameAsEr = (a: any) => a && decodePot(a.data).moves === erState.moves;
    rec.commit_visible_base_confirmed_ms = Math.round(await pollAccount(base, pot, sameAsEr, t0, 120000, "confirmed"));
    rec.commit_visible_base_finalized_ms = Math.round(await pollAccount(base, pot, sameAsEr, t0, 120000, "finalized", 250));
    try {
      const baseSig = await GetCommitmentSignature(c.sig, er);
      rec.commit_base_sig = baseSig; rec.commit_base_tx = await baseTxInfo(baseSig);
    } catch (e: any) { rec.commit_base_sig_err = String(e.message ?? e); }
    rec.base_owner_after_commit = (await base.getAccountInfo(pot))!.owner.toBase58();

    // One more move, then commit_and_undelegate. Return latency = sent -> base owner == program.
    await send(er, [ixMoveSeat(id, 2, 3, 5, payer.publicKey)], [payer]);
    const finalEr = decodePot((await er.getAccountInfo(pot))!.data);
    t0 = now();
    const u = await send(er, [ixClosePot(id)], [payer]);
    rec.undelegate_er_confirm_ms = Math.round(u.ms);
    const returned = (a: any) => a && a.owner.equals(PROGRAM_ID);
    rec.return_owner_base_confirmed_ms = Math.round(await pollAccount(base, pot, returned, t0, 180000, "confirmed"));
    rec.return_owner_base_finalized_ms = Math.round(await pollAccount(base, pot, returned, t0, 180000, "finalized", 250));
    try {
      const baseSig = await GetCommitmentSignature(u.sig, er);
      rec.undelegate_base_sig = baseSig; rec.undelegate_base_tx = await baseTxInfo(baseSig);
    } catch (e: any) { rec.undelegate_base_sig_err = String(e.message ?? e); }
    const back = decodePot((await base.getAccountInfo(pot))!.data);
    rec.base_state_after_return = fmtPot(back);
    rec.state_matches_er = back.moves === finalEr.moves && back.amounts.join() === finalEr.amounts.join();

    const co = await send(base, [ixCashOutCheck(id)], [payer]);
    rec.cashout_after_return = (await getLogs(base, co.sig)).filter((l) => /owner|returned/.test(l)).join(" | ");

    // ER view after undelegation: is the account still writable there?
    try { await send(er, [ixMoveSeat(id, 3, 0, 1, payer.publicKey)], [payer]); rec.er_write_after_undelegate = "UNEXPECTED_OK"; }
    catch (e: any) { rec.er_write_after_undelegate = String(e.message).split("\n")[0].slice(0, 200); }

    log(OUT, rec);
  }
}
main().catch((e) => { console.error(e); process.exit(1); });
