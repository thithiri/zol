// The session key: an Ed25519 key the page creates per app and keeps in localStorage.
// It signs the deposit memo and every Solana and rollup action of this player.
import { ed25519 } from "@noble/curves/ed25519";
import { sha256 } from "@noble/hashes/sha256";
import { Keypair } from "@solana/web3.js";

export interface KV {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
}

export interface Session {
  seed: Uint8Array; // 32-byte Ed25519 secret
  publicKey: Uint8Array; // 32 bytes
  keypair: Keypair; // same key, for Solana transactions
}

export const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
export const unhex = (s: string) => Uint8Array.from(s.match(/../g) ?? [], (h) => parseInt(h, 16));

export function sessionFromSeed(seed: Uint8Array): Session {
  return { seed, publicKey: ed25519.getPublicKey(seed), keypair: Keypair.fromSeed(seed) };
}

const sessionKey = (app: string) => `zol:session:${app}`;
const backedUpKey = (app: string, seedHex: string) => `zol:backedup:${app}:${seedHex.slice(0, 16)}`;

/** One session key per app program, created on first use. */
export function loadOrCreateSession(app: string, kv: KV): Session {
  const k = sessionKey(app);
  let s = kv.getItem(k);
  if (!s || s.length !== 64) {
    s = hex(ed25519.utils.randomPrivateKey());
    kv.setItem(k, s);
  }
  return sessionFromSeed(unhex(s));
}

export function sign(session: Session, msg: Uint8Array): Uint8Array {
  return ed25519.sign(msg, session.seed);
}

// ---------------------------------------------------------------- backup and restore
//
// The session key is the only way to play, exit or recover a credited balance: the ledger
// knows the player by it. It lives in this browser's storage, so a player must keep a copy.
// Format: `zolsess1:<app program>:<seed hex>:<checksum>`, the checksum being the first
// 4 bytes of sha256 over everything before it (catches a mistyped or truncated copy).

const PREFIX = "zolsess1";

function checksum(body: string): string {
  return hex(sha256(new TextEncoder().encode(body)).subarray(0, 4));
}

export function exportSession(app: string, s: Session): string {
  const body = `${PREFIX}:${app}:${hex(s.seed)}`;
  return `${body}:${checksum(body)}`;
}

/** Parses a backup string for `app`; throws with a reason a player can act on. */
export function parseSessionBackup(app: string, text: string): Session {
  const t = text.trim();
  const parts = t.split(":");
  if (parts.length !== 4 || parts[0] !== PREFIX) throw new Error("not a ZOL session backup (expected zolsess1:…)");
  const [, forApp, seedHex, sum] = parts;
  if (checksum(`${PREFIX}:${forApp}:${seedHex}`) !== sum) throw new Error("backup is damaged or mistyped (checksum mismatch)");
  if (forApp !== app) throw new Error(`backup is for app ${forApp}, not ${app}`);
  if (!/^[0-9a-f]{64}$/.test(seedHex)) throw new Error("backup key is malformed");
  return sessionFromSeed(unhex(seedHex));
}

/** Replaces this app's session key with the backed-up one (counted as backed up). */
export function restoreSession(app: string, text: string, kv: KV): Session {
  const s = parseSessionBackup(app, text);
  kv.setItem(sessionKey(app), hex(s.seed));
  markBackedUp(app, s, kv);
  return s;
}

export function isBackedUp(app: string, s: Session, kv: KV): boolean {
  return kv.getItem(backedUpKey(app, hex(s.seed))) === "1";
}

export function markBackedUp(app: string, s: Session, kv: KV) {
  kv.setItem(backedUpKey(app, hex(s.seed)), "1");
}
