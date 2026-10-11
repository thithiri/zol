// Instruction builders against the generated IDLs: discriminators, account order,
// signer and writable flags.
import { expect, test } from "vitest";
import { Keypair, PublicKey } from "@solana/web3.js";
import cfIdl from "../../idl/zol_coinflip.json";
import ledgerIdl from "../../idl/zol_ledger.json";
import { allowedStakes, Coinflip, decodeApp, decodeLedgerParams, decodeTable } from "../src/chain";
import { sha256 } from "@noble/hashes/sha256";

const cf = new Coinflip({ ledger: new PublicKey(ledgerIdl.address), app: new PublicKey(cfIdl.address) });
const payer = Keypair.generate().publicKey;
const session = Keypair.generate().publicKey;
const potId = new Uint8Array(32).fill(3);
const queue = Keypair.generate().publicKey;

function check(idl: any, name: string, ix: { data: Buffer; keys: { pubkey: PublicKey; isSigner: boolean; isWritable: boolean }[] }) {
  const d = idl.instructions.find((x: any) => x.name === name);
  expect(Array.from(ix.data.subarray(0, 8)), name).toEqual(d.discriminator);
  expect(ix.keys.length >= d.accounts.length, name).toBe(true);
  d.accounts.forEach((a: any, i: number) => {
    const k = ix.keys[i];
    expect(k.isSigner, `${name}.${a.name} signer`).toBe(!!a.signer);
    expect(k.isWritable, `${name}.${a.name} writable`).toBe(!!a.writable);
    if (a.address) expect(k.pubkey.toBase58(), `${name}.${a.name}`).toBe(a.address);
  });
}

test("coinflip instruction builders match idl/zol_coinflip.json", () => {
  check(cfIdl, "create_table", cf.createTable(payer, potId, 500_000n, 600n, payer));
  check(cfIdl, "join", cf.join(session, potId));
  check(cfIdl, "join_rounds", cf.join(session, potId, 10));
  expect(Array.from(cf.join(session, potId, 10).data.subarray(8))).toEqual([10, 0]);
  check(cfIdl, "start", cf.start(payer, potId, 600n));
  check(cfIdl, "flip", cf.flip(payer, potId, queue, new Uint8Array(32)));
  check(cfIdl, "finish", cf.finish(payer, potId));
  check(cfIdl, "finish_and_pay", cf.finishAndPay(payer, potId));
  check(cfIdl, "schedule_closer", cf.scheduleCloser(payer, potId));
  check(cfIdl, "cash_out_table", cf.cashOutTable(potId, payer, [session, payer]));
  check(cfIdl, "close_table", cf.closeTable(potId, payer));
  check(cfIdl, "rescue_table", cf.rescueTable(potId, payer));
});

test("ledger calls the page makes match idl/zol_ledger.json", () => {
  check(ledgerIdl, "cash_out", cf.cashOut(potId, [session, payer]));
  check(ledgerIdl, "request_exit", cf.requestExit(session));
});

test("Table decoder reads the Anchor layout", () => {
  const disc = cfIdl.accounts.find((a: any) => a.name === "Table")!.discriminator;
  const data = new Uint8Array(8 + 32 + 32 + 8 + 32 + 32 + 1 + 1 + 32 + 1 + 4 + 8 + 64 + 8 + 8);
  data.set(disc, 0);
  data.fill(7, 8, 40);
  data.set(potId, 40);
  new DataView(data.buffer).setBigUint64(72, 500_000n, true);
  data.fill(9, 80, 112); // validator
  data.fill(5, 112, 144); // creator
  data[144] = 3; // Landed
  data[145] = 1; // winner seat 1
  new DataView(data.buffer).setUint32(179, 7, true); // flips
  new DataView(data.buffer).setBigInt64(183, 1_791_000_000n, true); // flip_requested_at
  data.fill(4, 191, 223); // players[0]
  new DataView(data.buffer).setBigInt64(255, 1_791_001_800n, true); // session_deadline
  new DataView(data.buffer).setBigUint64(263, 0b101n, true); // results
  const t = decodeTable(data);
  expect(t.players[0].toBytes()).toEqual(new Uint8Array(32).fill(4));
  expect(t.sessionDeadline).toBe(1_791_001_800n);
  expect(t.results).toBe(0b101n);
  expect(decodeTable(data.subarray(0, 191)).players).toEqual([]);
  expect(t.flips).toBe(7);
  expect(t.flipRequestedAt).toBe(1_791_000_000n);
  // A table from before sessions (no flips fields) still decodes.
  expect(decodeTable(data.subarray(0, 179)).flips).toBe(0);
  expect(t.stake).toBe(500_000n);
  expect(t.state).toBe("Landed");
  expect(t.winner).toBe(1);
  expect(t.validator.toBytes()).toEqual(new Uint8Array(32).fill(9));
  expect(t.creator.toBytes()).toEqual(new Uint8Array(32).fill(5));
  expect(Array.from(t.potId)).toEqual(Array.from(potId));
});

test("stakes are one deposit's net value at the ledger's entry fee, and a tenth of it", () => {
  const two = [1_000_000n, 10_000_000n, 0n, 0n];
  expect(allowedStakes({ entryFeeBps: 30, denominations: two })).toEqual([997_000n, 9_970_000n, 99_700n]);
  expect(allowedStakes({ entryFeeBps: 50, denominations: two })).toEqual([995_000n, 9_950_000n, 99_500n]);
  expect(allowedStakes({ entryFeeBps: 30, denominations: [2_000_000n, 0n, 50_000_000n, 0n] })).toEqual([1_994_000n, 49_850_000n, 199_400n, 4_985_000n]);
  // Config layout: params after admin, attestor, 7 guardians and 4 bytes; denominations 69 bytes in.
  const disc = ledgerIdl.accounts.find((a: any) => a.name === "Config")!.discriminator;
  const data = new Uint8Array(420);
  data.set(disc, 0);
  const dv = new DataView(data.buffer);
  dv.setUint16(300, 30, true);
  dv.setBigUint64(369, 1_000_000n, true);
  dv.setBigUint64(377, 10_000_000n, true);
  dv.setBigUint64(312, 100_000n, true); // min_exit, after the two fees and the network allowance
  expect(decodeLedgerParams(data)).toEqual({ entryFeeBps: 30, denominations: two, minExit: 100_000n });
});

test("settle matches the ledger IDL; settlement PDA uses next_settlement", () => {
  const ix = cf.settle(payer, 7n, [session]);
  check(ledgerIdl, "settle", ix);
  const seq = new Uint8Array(8); new DataView(seq.buffer).setBigUint64(0, 7n, true);
  const [want] = PublicKey.findProgramAddressSync([Buffer.from("settlement"), cf.pdas.app().toBytes(), seq], new PublicKey(ledgerIdl.address));
  expect(ix.keys[3].pubkey.toBase58()).toBe(want.toBase58());
  expect(ix.keys[5].pubkey.toBase58()).toBe(cf.pdas.balance(session).toBase58());
  expect(ix.keys[5].isWritable).toBe(true);
});

test("decodeApp reads the App account layout", () => {
  const b = new Uint8Array(8 + 32 * 3 + 1 + 8 + 43 + 32 + 8 * 10 + 1);
  b.set(sha256(new TextEncoder().encode("account:App")).subarray(0, 8));
  const dv = new DataView(b.buffer);
  let o = 8 + 96;
  b[o] = 3; o += 1; // state
  dv.setBigInt64(o, 1234n, true); o += 8 + 43 + 32;
  const u = [5n, 997000n, 3n, 3000000n, 34982n, 2n, 1n, 0n]; // cap, liability, count, credited, fee_pool, next, paid, unpaid
  for (const v of u) { dv.setBigUint64(o, v, true); o += 8; }
  expect(decodeApp(b)).toEqual({ state: 3, activatesAt: 1234n, liability: 997000n, feePool: 34982n, nextSettlement: 2n, paidCount: 1n, unpaidTotal: 0n });
});
