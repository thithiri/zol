// The deposit flow, independent of the DOM: memo -> intent -> session UA -> ZIP 321 link.
import { depositLink, type DepositMemo } from "./wire";
import { buildSignedMemo, canonicalMemo, zecString } from "./memo";
import { hex, unhex, type Session } from "./session";
import { parsePayoutAddress } from "./ua";
import type { Gateway, AppInfo, SessionInfo } from "./gateway";

/** Blocks after the gateway's tip at which an uncredited deposit may be voided. */
export const EXPIRY_BLOCKS = 1_000;

export interface PreparedDeposit {
  app: AppInfo;
  memo: DepositMemo;
  session: SessionInfo;
  link: string;
  amountZat: bigint;
  /** Operators that accepted the intent, and the errors of those that did not. */
  registered: string[];
  failed: { gateway: string; error: string }[];
}

/**
 * Every operator decides from its own scan, so each needs the registered memo: a deposit
 * whose wallet dropped the memo is credited only if a threshold of operators hold the
 * intent. An operator passes an accepted intent on to its peers (best effort, in the
 * background), so registering with every operator directly is the reliable way. `gws` is
 * every operator's page API.
 */
export async function prepareDeposit(
  gws: Gateway | Gateway[], appProgram: string, s: Session, payoutInput: string, amountZat: bigint, autoExit = false,
): Promise<PreparedDeposit> {
  const all = Array.isArray(gws) ? gws : [gws];
  const gw = all[0];
  const app = await gw.app(appProgram);
  if (app.state !== "active") throw new Error(`app is ${app.state}, not active`);
  if (!app.denominations.map(BigInt).includes(amountZat)) throw new Error(`amount must be one of ${app.denominations.join(", ")} zatoshi`);
  const payoutAddress = parsePayoutAddress(payoutInput, app.network);
  const memo = buildSignedMemo({
    session: s, payoutAddress, vaultAddress: unhex(app.vault_address), network: app.network,
    expiryHeight: app.tip + EXPIRY_BLOCKS, autoExit,
  });
  const memoHex = hex(canonicalMemo(memo));
  const results = await Promise.allSettled(all.map((g) => g.registerIntent(appProgram, memoHex)));
  const registered: string[] = [];
  const failed: { gateway: string; error: string }[] = [];
  let session: SessionInfo | undefined;
  results.forEach((r, i) => {
    if (r.status === "fulfilled" && r.value.intent_registered) {
      registered.push(all[i].base);
      session ??= r.value;
    } else {
      failed.push({ gateway: all[i].base, error: r.status === "rejected" ? String(r.reason?.message ?? r.reason) : "not registered" });
    }
  });
  if (!session) throw new Error(`no gateway registered the intent: ${failed.map((f) => f.error).join("; ")}`);
  // Every operator must derive the same session receiver.
  for (const r of results) if (r.status === "fulfilled" && r.value.ua !== session.ua) throw new Error("operators disagree on the session address");
  return { app, memo, session, link: depositLink(session.ua, zecString(amountZat), memo), amountZat, registered, failed };
}
