import { describe, expect, it } from "vitest";
import { exportSession, isBackedUp, loadOrCreateSession, markBackedUp, parseSessionBackup, restoreSession, hex } from "../src/session";

const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const mem = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
};

describe("session backup", () => {
  it("round-trips and restores the same key", () => {
    const a = loadOrCreateSession(APP, mem());
    const b = exportSession(APP, a);
    expect(b).toMatch(new RegExp(`^zolsess1:${APP}:[0-9a-f]{64}:[0-9a-f]{8}$`));
    const kv = mem();
    const r = restoreSession(APP, `  ${b}\n`, kv); // surrounding whitespace from a paste is fine
    expect(hex(r.publicKey)).toBe(hex(a.publicKey));
    expect(hex(loadOrCreateSession(APP, kv).publicKey)).toBe(hex(a.publicKey));
    expect(isBackedUp(APP, r, kv)).toBe(true);
  });

  it("rejects a mistyped, foreign or malformed backup", () => {
    const b = exportSession(APP, loadOrCreateSession(APP, mem()));
    const flipped = b.slice(0, 30) + (b[30] === "a" ? "b" : "a") + b.slice(31);
    expect(() => parseSessionBackup(APP, flipped)).toThrow(/checksum/);
    expect(() => parseSessionBackup("OtherApp1111111111111111111111111111111111", b)).toThrow(/for app/);
    expect(() => parseSessionBackup(APP, "hello")).toThrow(/not a ZOL session backup/);
    expect(() => parseSessionBackup(APP, b.slice(0, -2))).toThrow(/checksum/);
  });

  it("tracks backup per key", () => {
    const kv = mem();
    const s = loadOrCreateSession(APP, kv);
    expect(isBackedUp(APP, s, kv)).toBe(false);
    markBackedUp(APP, s, kv);
    expect(isBackedUp(APP, s, kv)).toBe(true);
  });
});
