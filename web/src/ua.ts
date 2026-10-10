// Payout address parsing: a Zcash unified address (ZIP 316: bech32m + F4Jumble + TLV
// items) or a raw 43-byte Orchard-format receiver in hex. Ironwood uses the same
// receivers, so the Orchard item (typecode 0x03) is what the memo carries.
import { blake2b } from "@noble/hashes/blake2b";
import { bech32m } from "@scure/base";

export type ZNetwork = "mainnet" | "testnet" | "regtest";
export const UA_HRP: Record<ZNetwork, string> = { mainnet: "u", testnet: "utest", regtest: "uregtest" };
const ORCHARD_TYPECODE = 0x03;
const RECEIVER_LEN = 43;

const enc = new TextEncoder();

function personal(tag: string, ...rest: number[]): Uint8Array {
  const p = new Uint8Array(16);
  p.set(enc.encode(tag));
  p.set(rest, tag.length);
  return p;
}

function xor(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length);
  for (let i = 0; i < a.length; i++) out[i] = a[i] ^ b[i];
  return out;
}

// ZIP 316, F4Jumble. H_i: BLAKE2b-(8·ℓL) with personalization "UA_F4Jumble_H" || i || 0 || 0.
const H = (i: number, u: Uint8Array, lL: number) => blake2b(u, { dkLen: lL, personalization: personal("UA_F4Jumble_H", i, 0, 0) });
// G_i: the first ℓR bytes of BLAKE2b-512("UA_F4Jumble_G" || i || j as u16 LE, u) for j = 0, 1, ...
function G(i: number, u: Uint8Array, lR: number): Uint8Array {
  const out = new Uint8Array(lR);
  for (let j = 0; j * 64 < lR; j++) {
    const block = blake2b(u, { dkLen: 64, personalization: personal("UA_F4Jumble_G", i, j & 0xff, j >> 8) });
    out.set(block.subarray(0, Math.min(64, lR - j * 64)), j * 64);
  }
  return out;
}

function lengths(n: number): [number, number] {
  if (n < 48 || n > 4194368) throw new Error("F4Jumble: bad length");
  const lL = Math.min(64, Math.floor(n / 2));
  return [lL, n - lL];
}

export function f4jumble(m: Uint8Array): Uint8Array {
  const [lL, lR] = lengths(m.length);
  const a = m.subarray(0, lL);
  const b = m.subarray(lL);
  const x = xor(b, G(0, a, lR));
  const y = xor(a, H(0, x, lL));
  const d = xor(x, G(1, y, lR));
  const c = xor(y, H(1, d, lL));
  const out = new Uint8Array(m.length);
  out.set(c, 0);
  out.set(d, lL);
  return out;
}

export function f4jumbleInv(m: Uint8Array): Uint8Array {
  const [lL, lR] = lengths(m.length);
  const c = m.subarray(0, lL);
  const d = m.subarray(lL);
  const y = xor(c, H(1, d, lL));
  const x = xor(d, G(1, y, lR));
  const a = xor(y, H(0, x, lL));
  const b = xor(x, G(0, a, lR));
  const out = new Uint8Array(m.length);
  out.set(a, 0);
  out.set(b, lL);
  return out;
}

function compactSize(buf: Uint8Array, i: number): [number, number] {
  const f = buf[i];
  if (f === undefined) throw new Error("UA: truncated");
  if (f < 0xfd) return [f, i + 1];
  if (f === 0xfd) {
    const v = buf[i + 1] | (buf[i + 2] << 8);
    if (v < 0xfd) throw new Error("UA: non-canonical compactSize");
    return [v, i + 3];
  }
  if (f === 0xfe) {
    const v = (buf[i + 1] | (buf[i + 2] << 8) | (buf[i + 3] << 16)) + buf[i + 4] * 2 ** 24;
    if (v < 0x10000) throw new Error("UA: non-canonical compactSize");
    return [v, i + 5];
  }
  throw new Error("UA: compactSize too large");
}

export interface UaItem {
  typecode: number;
  value: Uint8Array;
}

/** Decodes a unified address into its network and items (ZIP 316 revision 0). */
export function decodeUa(ua: string): { network: ZNetwork; items: UaItem[] } {
  const s = ua.trim();
  if (s !== s.toLowerCase() && s !== s.toUpperCase()) throw new Error("UA: mixed case");
  const { prefix, words } = bech32m.decode(s.toLowerCase() as `${string}1${string}`, 10_000);
  const network = (Object.keys(UA_HRP) as ZNetwork[]).find((n) => UA_HRP[n] === prefix);
  if (!network) throw new Error(`UA: unknown prefix "${prefix}"`);
  const raw = f4jumbleInv(Uint8Array.from(bech32m.fromWords(words)));
  // The last 16 bytes are the HRP, zero padded.
  const pad = new Uint8Array(16);
  pad.set(enc.encode(prefix));
  const body = raw.subarray(0, raw.length - 16);
  if (!raw.subarray(raw.length - 16).every((v, i) => v === pad[i])) throw new Error("UA: bad padding");
  const items: UaItem[] = [];
  let i = 0;
  let last = -1;
  while (i < body.length) {
    let typecode: number, len: number;
    [typecode, i] = compactSize(body, i);
    [len, i] = compactSize(body, i);
    if (i + len > body.length) throw new Error("UA: truncated item");
    if (typecode <= last) throw new Error("UA: items out of order");
    last = typecode;
    items.push({ typecode, value: body.slice(i, i + len) });
    i += len;
  }
  return { network, items };
}

/** The 43-byte Orchard-format receiver of a unified address. */
export function orchardReceiverFromUa(ua: string, expected?: ZNetwork): Uint8Array {
  const { network, items } = decodeUa(ua);
  if (expected && network !== expected) throw new Error(`UA is for ${network}, the app runs on ${expected}`);
  const o = items.find((it) => it.typecode === ORCHARD_TYPECODE);
  if (!o) throw new Error("UA has no Orchard receiver (needed for Ironwood payouts)");
  if (o.value.length !== RECEIVER_LEN) throw new Error("UA: Orchard receiver is not 43 bytes");
  return o.value;
}

/** Accepts a unified address or 86 hex characters of a raw receiver. */
export function parsePayoutAddress(input: string, expected?: ZNetwork): Uint8Array {
  const s = input.trim();
  if (/^[0-9a-fA-F]{86}$/.test(s)) return Uint8Array.from(s.match(/../g)!.map((h) => parseInt(h, 16)));
  return orchardReceiverFromUa(s, expected);
}

/** Encodes an Orchard-only unified address (for tests and display). */
export function encodeOrchardUa(receiver: Uint8Array, network: ZNetwork): string {
  if (receiver.length !== RECEIVER_LEN) throw new Error("receiver must be 43 bytes");
  const hrp = UA_HRP[network];
  const body = new Uint8Array(2 + RECEIVER_LEN + 16);
  body.set([ORCHARD_TYPECODE, RECEIVER_LEN], 0);
  body.set(receiver, 2);
  body.set(enc.encode(hrp), 2 + RECEIVER_LEN);
  return bech32m.encode(hrp, bech32m.toWords(f4jumble(body)), 10_000);
}
