// sendTx: rebroadcasts until confirmed; expiry and preflight failures are reported plainly.
import { expect, test, vi } from "vitest";
import { Keypair, SystemProgram } from "@solana/web3.js";
import { sendTx } from "../src/chain";

function fakeConn(opts: { confirmAfter?: number; expireAfter?: number; preflight?: Error }) {
  let sends = 0;
  let polls = 0;
  return {
    sends: () => sends,
    conn: {
      getLatestBlockhash: async () => ({ blockhash: Keypair.generate().publicKey.toBase58(), lastValidBlockHeight: 100 }),
      sendRawTransaction: async () => {
        sends++;
        if (opts.preflight && sends === 1) throw opts.preflight;
        return "sig";
      },
      getSignatureStatuses: async () => {
        polls++;
        return { value: [opts.confirmAfter !== undefined && polls > opts.confirmAfter ? { err: null, confirmationStatus: "confirmed" } : null] };
      },
      getBlockHeight: async () => (opts.expireAfter !== undefined && polls > opts.expireAfter ? 101 : 50),
      getTransaction: async () => null,
    } as any,
  };
}
const ix = SystemProgram.transfer({ fromPubkey: Keypair.generate().publicKey, toPubkey: Keypair.generate().publicKey, lamports: 1 });

test("rebroadcasts until confirmed", async () => {
  vi.useFakeTimers();
  const f = fakeConn({ confirmAfter: 2 });
  const payer = Keypair.generate();
  const p = sendTx(f.conn, [{ ...ix, keys: [{ ...ix.keys[0], pubkey: payer.publicKey }, ix.keys[1]] }], payer);
  await vi.runAllTimersAsync();
  expect(await p).toBe("sig");
  expect(f.sends()).toBe(3); // first send plus two rebroadcasts
  vi.useRealTimers();
});

test("reports expiry instead of hanging", async () => {
  vi.useFakeTimers();
  const f = fakeConn({ expireAfter: 1 });
  const payer = Keypair.generate();
  const p = sendTx(f.conn, [{ ...ix, keys: [{ ...ix.keys[0], pubkey: payer.publicKey }, ix.keys[1]] }], payer);
  const r = expect(p).rejects.toThrow(/expired before it landed/);
  await vi.runAllTimersAsync();
  await r;
  vi.useRealTimers();
});

test("a preflight failure surfaces its reason", async () => {
  const f = fakeConn({ preflight: Object.assign(new Error("Simulation failed"), { transactionMessage: "Attempt to debit an account but found no record of a prior credit." }) });
  const payer = Keypair.generate();
  await expect(sendTx(f.conn, [{ ...ix, keys: [{ ...ix.keys[0], pubkey: payer.publicKey }, ix.keys[1]] }], payer)).rejects.toThrow(/no record of a prior credit/);
});
