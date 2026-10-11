import { expect, test } from "vitest";
import { prfSalt, seedFromPrf } from "../src/passkey";
import { hex } from "../src/session";

const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";

test("the passkey seed is deterministic, per app, and depends on the PRF output", () => {
  const prf = new Uint8Array(32).fill(7);
  expect(hex(seedFromPrf(APP, prf))).toBe(hex(seedFromPrf(APP, prf)));
  expect(hex(seedFromPrf(APP, prf))).not.toBe(hex(seedFromPrf("OtherApp111111111111111111111111111111111111", prf)));
  expect(hex(seedFromPrf(APP, prf))).not.toBe(hex(seedFromPrf(APP, new Uint8Array(32).fill(8))));
  expect(seedFromPrf(APP, prf).length).toBe(32);
  // The salt is fixed per app (a different salt would give a different session).
  expect(hex(prfSalt(APP))).toBe(hex(prfSalt(APP)));
  expect(hex(prfSalt(APP))).not.toBe(hex(prfSalt("OtherApp111111111111111111111111111111111111")));
});
