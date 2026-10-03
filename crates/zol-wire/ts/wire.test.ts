import { expect, test } from "bun:test";
import { createPrivateKey, sign } from "node:crypto";
import { creditDigest, depositLink, encodeMemo, encodeMemoText, memoSigningMessage } from "./wire";
import v from "../vectors/v0.3.json";

const hex = (s: string) => Uint8Array.from(Buffer.from(s, "hex"));
const toHex = (b: Uint8Array) => Buffer.from(b).toString("hex");
const i = v.inputs;

test("memo, signature and credit digest match Rust vectors", () => {
  const memo = {
    flags: i.flags, sessionKey: hex(i.session_key), payoutAddress: hex(i.payout_address),
    expiryHeight: i.expiry_height, potId: new Uint8Array(32), signature: new Uint8Array(64),
    appData: new TextEncoder().encode(i.app_data_utf8),
  };
  const msg = memoSigningMessage(memo, i.network, hex(i.vault_address));
  expect(toHex(msg)).toBe(v.memo_signing_message);
  // Sign with the session secret (PKCS#8 wrapper for a raw Ed25519 seed).
  const pkcs8 = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(i.session_secret, "hex")]);
  memo.signature = new Uint8Array(sign(null, msg, createPrivateKey({ key: pkcs8, format: "der", type: "pkcs8" })));
  expect(toHex(encodeMemo(memo))).toBe(v.memo);
  expect(encodeMemoText(memo)).toBe(v.memo_text);
  const link = depositLink("utest1vault", "0.1", memo);
  expect(link.length).toBeLessThan(800);
  const d = creditDigest({
    network: i.network, app: hex(i.app), depositId: hex(v.deposit_id), amount: BigInt(i.amount),
    sessionKey: hex(i.session_key), payoutCommitment: hex(v.payout_commitment), flags: i.flags,
    potId: new Uint8Array(32), appData: memo.appData,
  });
  expect(toHex(d)).toBe(v.credit_digest);
});
