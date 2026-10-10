// Builds and signs the v2 deposit memo (crates/zol-wire; TypeScript twin in ./wire.ts).
import { encodeMemo, FLAG_AUTO_EXIT, memoSigningMessage, type DepositMemo } from "./wire";
import { sign, type Session } from "./session";
import type { ZNetwork } from "./ua";

export const NETWORK_BYTE: Record<ZNetwork, number> = { mainnet: 0, testnet: 1, regtest: 2 };

export interface MemoInput {
  session: Session;
  payoutAddress: Uint8Array; // 43-byte receiver
  vaultAddress: Uint8Array; // 43-byte receiver the memo signs (AppInfo.vault_address)
  network: ZNetwork;
  expiryHeight: number;
  autoExit?: boolean;
  appData?: Uint8Array;
}

export function buildSignedMemo(i: MemoInput): DepositMemo {
  if (i.payoutAddress.length !== 43 || i.vaultAddress.length !== 43) throw new Error("receivers are 43 bytes");
  const memo: DepositMemo = {
    flags: i.autoExit ? FLAG_AUTO_EXIT : 0,
    sessionKey: i.session.publicKey,
    payoutAddress: i.payoutAddress,
    expiryHeight: i.expiryHeight,
    potId: new Uint8Array(32), // reserved for deposit-as-join
    signature: new Uint8Array(64),
    appData: i.appData ?? new Uint8Array(0),
  };
  memo.signature = sign(i.session, memoSigningMessage(memo, NETWORK_BYTE[i.network], i.vaultAddress));
  return memo;
}

export const canonicalMemo = (m: DepositMemo) => encodeMemo(m);

/** zatoshi -> decimal ZEC string for ZIP 321 (no trailing zeros). */
export function zecString(zat: bigint): string {
  const whole = zat / 100_000_000n;
  const frac = (zat % 100_000_000n).toString().padStart(8, "0").replace(/0+$/, "");
  return frac ? `${whole}.${frac}` : `${whole}`;
}
