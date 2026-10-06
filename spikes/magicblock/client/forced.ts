// Owner-program forced undelegation (DLP RequestUndelegation=26, UndelegateWithRollbackAfterTimeout=27).
// Needs the devnet DLP binary (DLP=devnet ./start-local.sh); mainnet's DLP lacks these instructions.
// Usage: bun client/forced.ts honest | dead
import { execSync } from "node:child_process";
import {
  base, er, payer, PROGRAM_ID, potPda, undelRequestPda, decodePot, fmtPot,
  ixInitPot, ixDelegatePot, ixMoveSeat, ixCommitPot, ixClosePot, ixCashOutCheck, ixRequestUndel, ixRollback,
  send, getLogs, pollAccount, validatorIdentity, log, now, sleep,
} from "./common.ts";

const OUT = "forced.jsonl";
const mode = process.argv[2] ?? "honest";
const short = (e: any) => String(e?.message ?? e).split("\n")[0].slice(0, 300);
const logsOf = (e: any) => (e?.logs ?? []).filter((l: string) => !/consumed|invoke \[|success$/.test(l)).slice(-5);
const owner = async (k: any) => (await base.getAccountInfo(k))?.owner.toBase58();
const ER_PID = () => execSync(`pgrep -f "ephemeral-validator-darwin-arm64/bin/ephemeral-validator --no-tui --listen 127.0.0.1:27799"`).toString().trim().split("\n")[0];

async function setup(validator: any) {
  const id = BigInt(Date.now()) * 1000n + BigInt(Math.floor(Math.random() * 1000));
  await send(base, [ixInitPot(id, [100, 100, 100, 100])], [payer]);
  await send(base, [ixDelegatePot(id, validator)], [payer]);
  await pollAccount(er, potPda(id), (a) => a && a.owner.equals(PROGRAM_ID), now(), 30000);
  return id;
}
async function readRequest(pot: any) {
  const a = await base.getAccountInfo(undelRequestPda(pot));
  if (!a) return null;
  // UndelegationRequest { delegated_account: Pubkey, expires_at_slot: u64 } after an 8-byte discriminator
  return { len: a.data.length, expires_at_slot: Number(a.data.readBigUInt64LE(8 + 32)), owner: a.owner.toBase58() };
}

async function honest() {
  const validator = await validatorIdentity();
  const id = await setup(validator); const pot = potPda(id);
  const r: Record<string, unknown> = { event: "forced_honest", pot: pot.toBase58() };
  await send(er, [ixMoveSeat(id, 0, 1, 25, payer.publicKey)], [payer]);
  r.er_state = fmtPot(decodePot((await er.getAccountInfo(pot))!.data));
  const req = await send(base, [ixRequestUndel(id)], [payer]);
  r.request_sig = req.sig; r.request_slot = await base.getSlot(); r.request = await readRequest(pot);
  const tReq = now() - req.ms;
  const back = await pollAccount(base, pot, (a) => a && a.owner.equals(PROGRAM_ID), tReq, 60000);
  r.auto_return_after_request_ms = Math.round(back); // measured from request tx send
  r.auto_return_state = back > 0 ? fmtPot(decodePot((await base.getAccountInfo(pot))!.data)) : await owner(pot);
  // A plain commit after the request: the ER may accept the intent, but DLP rejects it on base.
  try { await send(er, [ixMoveSeat(id, 1, 2, 5, payer.publicKey)], [payer]); const c = await send(er, [ixCommitPot(id)], [payer]); r.plain_commit_er = "accepted " + c.sig; }
  catch (e) { r.plain_commit_er = short(e); r.plain_commit_logs = logsOf(e); }
  const want = decodePot((await er.getAccountInfo(pot))!.data);
  r.plain_commit_landed_on_base_ms = Math.round(await pollAccount(base, pot, (a) => a && decodePot(a.data).moves === want.moves, now(), 15000));
  // commit_and_undelegate still works.
  const t0 = now();
  try { await send(er, [ixClosePot(id)], [payer]); r.close = "ok"; } catch (e) { r.close = short(e); r.close_logs = logsOf(e); }
  r.close_return_ms = Math.round(await pollAccount(base, pot, (a) => a && a.owner.equals(PROGRAM_ID), t0, 60000));
  if (r.close_return_ms as number > 0) {
    r.base_state = fmtPot(decodePot((await base.getAccountInfo(pot))!.data));
    r.request_pda_after = await readRequest(pot);
    const co = await send(base, [ixCashOutCheck(id)], [payer]); r.cashout = (await getLogs(base, co.sig)).filter((l) => /returned/.test(l)).join();
  }
  log(OUT, r);
}

async function dead() {
  const validator = await validatorIdentity();
  const id = await setup(validator); const pot = potPda(id);
  const r: Record<string, unknown> = { event: "forced_dead_validator", pot: pot.toBase58() };
  await send(er, [ixMoveSeat(id, 0, 1, 30, payer.publicKey)], [payer]);
  const committed = decodePot((await er.getAccountInfo(pot))!.data);
  await send(er, [ixCommitPot(id)], [payer]);
  r.commit_landed_ms = Math.round(await pollAccount(base, pot, (a) => a && decodePot(a.data).moves === committed.moves, now(), 60000));
  await send(er, [ixMoveSeat(id, 1, 2, 7, payer.publicKey)], [payer]);
  r.committed_state = fmtPot(committed);
  r.uncommitted_er_state = fmtPot(decodePot((await er.getAccountInfo(pot))!.data));

  // Freeze the ER validator (SIGSTOP) to model a validator that stops returning the pot.
  const pid = ER_PID(); execSync(`kill -STOP ${pid}`); r.er_pid_stopped = pid;
  // Rollback before the request exists / before timeout must fail.
  try { await send(base, [ixRollback(id, validator)], [payer]); r.rollback_without_request = "UNEXPECTED_OK"; } catch (e) { r.rollback_without_request = logsOf(e).join(" / ").slice(0, 300); }
  const req = await send(base, [ixRequestUndel(id)], [payer]);
  const rq = await readRequest(pot); r.request = rq; r.request_slot = await base.getSlot();
  try { await send(base, [ixRollback(id, validator)], [payer]); r.rollback_before_timeout = "UNEXPECTED_OK"; } catch (e) { r.rollback_before_timeout = logsOf(e).join(" / ").slice(0, 300); }
  log(OUT, { event: "forced_dead_waiting", ...r });
  const tWait = now();
  while ((await base.getSlot()) < rq!.expires_at_slot) await sleep(5000);
  r.waited_s = Math.round((now() - tWait) / 1000);
  const rb = await send(base, [ixRollback(id, validator)], [payer]);
  r.rollback_logs = (await getLogs(base, rb.sig)).filter((l) => /rolled back|Error/.test(l));
  const a = (await base.getAccountInfo(pot))!;
  r.owner_after_rollback = a.owner.toBase58();
  r.state_after_rollback = fmtPot(decodePot(a.data));
  r.equals_last_commit = decodePot(a.data).moves === committed.moves;
  const co = await send(base, [ixCashOutCheck(id)], [payer]); r.cashout = (await getLogs(base, co.sig)).filter((l) => /returned/.test(l)).join();
  // Resume the ER: what does it do with an account the base layer took back?
  execSync(`kill -CONT ${pid}`);
  await sleep(8000);
  try { await send(er, [ixMoveSeat(id, 2, 3, 1, payer.publicKey)], [payer]); r.er_write_after_rollback = "ok (ER still thinks it is delegated)"; } catch (e) { r.er_write_after_rollback = short(e); }
  try { await send(er, [ixClosePot(id)], [payer]); r.er_close_after_rollback = "ER accepted"; } catch (e) { r.er_close_after_rollback = short(e); }
  await sleep(10000);
  r.base_state_10s_after_er_resume = fmtPot(decodePot((await base.getAccountInfo(pot))!.data));
  r.base_owner_10s_after_er_resume = await owner(pot);
  log(OUT, r);
}

(mode === "dead" ? dead() : honest()).catch((e) => { console.error(e); process.exit(1); });
