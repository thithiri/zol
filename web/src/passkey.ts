// A session key held by a passkey (WebAuthn PRF extension).
//
// Passkeys sign with P-256, but the ledger and the rollup want Ed25519 signatures from the
// session key. So the passkey is used to *derive* the session seed instead: WebAuthn's PRF
// extension returns a secret that is fixed for (passkey, salt), and
//   seed = SHA-256("ZOL passkey session v1" || app || prf)
// The same passkey gives the same session on any device it syncs to (iCloud Keychain,
// Google Password Manager, 1Password, …): no backup string to keep. Without PRF support the
// page falls back to a random session and its backup string.
import { sha256 } from "@noble/hashes/sha256";

const enc = new TextEncoder();
const CRED_KEY = (app: string) => `zol:passkey:${app}`;

/** The PRF input: fixed per app, so the passkey always yields the same secret for it. */
export function prfSalt(app: string): Uint8Array {
  return sha256(enc.encode(`zol/session/v1/${app}`));
}

/** The session seed from the passkey's PRF output (32 bytes). */
export function seedFromPrf(app: string, prf: Uint8Array): Uint8Array {
  const h = sha256.create();
  h.update(enc.encode("ZOL passkey session v1"));
  h.update(enc.encode(app));
  h.update(prf);
  return h.digest();
}

export function passkeysAvailable(): boolean {
  return typeof window !== "undefined" && !!window.PublicKeyCredential && !!navigator.credentials;
}

const b64 = (b: ArrayBuffer | Uint8Array) => btoa(String.fromCharCode(...new Uint8Array(b as ArrayBuffer)));
const unb64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

function prfFirst(cred: PublicKeyCredential): Uint8Array | null {
  const r: any = cred.getClientExtensionResults();
  const first = r?.prf?.results?.first;
  return first ? new Uint8Array(first) : null;
}

/** Asks an existing passkey (any on this site, or `credentialId`) for the session seed. */
export async function seedFromPasskey(app: string, credentialId?: string): Promise<Uint8Array> {
  const cred = (await navigator.credentials.get({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rpId: location.hostname,
      userVerification: "required",
      ...(credentialId ? { allowCredentials: [{ type: "public-key", id: unb64(credentialId) }] } : {}),
      extensions: { prf: { eval: { first: prfSalt(app) } } } as any,
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("no passkey chosen");
  const prf = prfFirst(cred);
  if (!prf) throw new Error("this passkey or browser does not support the PRF extension; use a backup key instead");
  localStorage.setItem(CRED_KEY(app), b64(cred.rawId));
  return seedFromPrf(app, prf);
}

/** Creates a passkey for a new player, then derives the session seed from it. */
export async function createPasskeySeed(app: string): Promise<Uint8Array> {
  const cred = (await navigator.credentials.create({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rp: { name: "ZOL coin flip", id: location.hostname },
      user: { id: crypto.getRandomValues(new Uint8Array(16)), name: `ZOL player ${new Date().toISOString().slice(0, 10)}`, displayName: "ZOL player" },
      pubKeyCredParams: [{ type: "public-key", alg: -7 }, { type: "public-key", alg: -257 }],
      authenticatorSelection: { residentKey: "required", userVerification: "required" },
      extensions: { prf: { eval: { first: prfSalt(app) } } } as any,
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("passkey not created");
  const ext: any = cred.getClientExtensionResults();
  if (ext?.prf?.enabled === false) throw new Error("this browser or password manager can't derive keys from passkeys (no PRF); use a backup key instead");
  localStorage.setItem(CRED_KEY(app), b64(cred.rawId));
  // Some platforms return the PRF output at creation; most need one more prompt.
  const prf = prfFirst(cred);
  return prf ? seedFromPrf(app, prf) : seedFromPasskey(app, b64(cred.rawId));
}
