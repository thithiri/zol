// TypeScript twin of zol-wire's deposit memo and credit digest, for the web page.
// Byte-exact with crates/zol-wire; checked against vectors/v0.3.json.
// No imports: runs unchanged in browsers (web/ symlinks this file), bun and node >= 20.

export const MEMO_LEN = 512;
export const APP_DATA_MAX = 329;
export const FLAG_AUTO_EXIT = 0x01;
const enc = new TextEncoder();
const tag16 = (s: string) => {
  const t = new Uint8Array(16);
  t.set(enc.encode(s));
  return t;
};
export const TAGS = { memoSig: tag16("ZOL/memo-sig/v2"), credit: tag16("ZOL/credit/v1") };

export interface DepositMemo {
  flags: number;
  sessionKey: Uint8Array; // 32
  payoutAddress: Uint8Array; // 43, raw Orchard-format receiver (Ironwood)
  expiryHeight: number; // u32
  potId: Uint8Array; // 32
  signature: Uint8Array; // 64
  appData: Uint8Array; // <= 329
}

const le32 = (n: number) => {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, n, true);
  return b;
};
const le64 = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, true);
  return b;
};
const concat = (...parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((a, p) => a + p.length, 0));
  let i = 0;
  for (const p of parts) {
    out.set(p, i);
    i += p.length;
  }
  return out;
};

export function encodeMemo(m: DepositMemo): Uint8Array {
  if (m.appData.length > APP_DATA_MAX) throw new Error("app data too long");
  if (m.flags & ~FLAG_AUTO_EXIT) throw new Error("reserved flags");
  const out = new Uint8Array(MEMO_LEN);
  out.set([0xff, 0x5a, 0x4f, 0x4c, 0x02, m.flags], 0);
  out.set(m.sessionKey, 6);
  out.set(m.payoutAddress, 38);
  out.set(le32(m.expiryHeight), 81);
  out.set(m.potId, 85);
  out.set(m.signature, 117);
  out.set([m.appData.length & 0xff, m.appData.length >> 8], 181);
  out.set(m.appData, 183);
  return out;
}

/** Bytes the session key signs (Ed25519, no prehash). */
export function memoSigningMessage(m: DepositMemo, network: number, vaultAddress: Uint8Array): Uint8Array {
  const e = encodeMemo(m);
  return concat(TAGS.memoSig, new Uint8Array([network]), vaultAddress, e.subarray(0, 117), e.subarray(181));
}

/** SHA-256 credit digest (WebCrypto, hence async). */
export async function creditDigest(a: {
  network: number; app: Uint8Array; depositId: Uint8Array; amount: bigint; sessionKey: Uint8Array;
  payoutCommitment: Uint8Array; flags: number; potId: Uint8Array; appData: Uint8Array;
}): Promise<Uint8Array> {
  const len = new Uint8Array([a.appData.length & 0xff, a.appData.length >> 8]);
  const bytes = concat(TAGS.credit, new Uint8Array([a.network]), a.app, a.depositId, le64(a.amount),
    a.sessionKey, a.payoutCommitment, new Uint8Array([a.flags]), a.potId, len, a.appData);
  return new Uint8Array(await globalThis.crypto.subtle.digest("SHA-256", bytes));
}

/** Unpadded base64url (RFC 4648 section 5). */
export function base64url(b: Uint8Array): string {
  let s = "";
  for (const x of b) s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Text transport form ("ZOL1:" + base64url of the canonical memo minus its 0xFF byte,
 *  trailing zeros stripped). Wallets treat it as a UTF-8 text memo. */
export const TEXT_MAX = 511;
export function encodeMemoText(m: DepositMemo): string {
  const canon = encodeMemo(m);
  let end = canon.length;
  while (end > 1 && canon[end - 1] === 0) end--;
  const text = "ZOL1:" + base64url(canon.subarray(1, end));
  if (text.length > TEXT_MAX) throw new Error("memo too long for the text form");
  return text;
}

/** ZIP 321 payment link for a deposit. `memo` is the base64url of the text memo bytes. */
export function depositLink(vaultUa: string, amountZec: string, m: DepositMemo): string {
  const memo = base64url(enc.encode(encodeMemoText(m)));
  return `zcash:${vaultUa}?amount=${amountZec}&memo=${memo}`;
}
