// The session key: an Ed25519 key the page creates per app and keeps in localStorage.
// It signs the deposit memo and every Solana and rollup action of this player.
import { ed25519 } from "@noble/curves/ed25519";
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

/** One session key per app program, created on first use. */
export function loadOrCreateSession(app: string, kv: KV): Session {
  const k = `zol:session:${app}`;
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
