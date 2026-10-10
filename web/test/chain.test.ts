// Instruction builders against the generated IDLs: discriminators, account order,
// signer and writable flags.
import { expect, test } from "vitest";
import { Keypair, PublicKey } from "@solana/web3.js";
import cfIdl from "../../idl/zol_coinflip.json";
import ledgerIdl from "../../idl/zol_ledger.json";
import { allowedStakes, Coinflip, decodeEntryFeeBps, decodeTable } from "../src/chain";

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
  check(cfIdl, "start", cf.start(payer, potId, 600n));
  check(cfIdl, "flip", cf.flip(payer, potId, queue, new Uint8Array(32)));
  check(cfIdl, "finish", cf.finish(payer, potId));
  check(cfIdl, "close_table", cf.closeTable(potId, payer));
});

test("ledger calls the page makes match idl/zol_ledger.json", () => {
  check(ledgerIdl, "cash_out", cf.cashOut(potId, [session, payer]));
  check(ledgerIdl, "request_exit", cf.requestExit(session));
});

test("Table decoder reads the Anchor layout", () => {
  const disc = cfIdl.accounts.find((a: any) => a.name === "Table")!.discriminator;
  const data = new Uint8Array(8 + 32 + 32 + 8 + 32 + 32 + 1 + 1 + 32 + 1);
  data.set(disc, 0);
  data.fill(7, 8, 40);
  data.set(potId, 40);
  new DataView(data.buffer).setBigUint64(72, 500_000n, true);
  data.fill(9, 80, 112); // validator
  data.fill(5, 112, 144); // creator
  data[144] = 3; // Landed
  data[145] = 1; // winner seat 1
  const t = decodeTable(data);
  expect(t.stake).toBe(500_000n);
  expect(t.state).toBe("Landed");
  expect(t.winner).toBe(1);
  expect(t.validator.toBytes()).toEqual(new Uint8Array(32).fill(9));
  expect(t.creator.toBytes()).toEqual(new Uint8Array(32).fill(5));
  expect(Array.from(t.potId)).toEqual(Array.from(potId));
});

test("stakes are one deposit's net value at the ledger's entry fee", () => {
  expect(allowedStakes(30)).toEqual([997_000n, 9_970_000n]);
  expect(allowedStakes(50)).toEqual([995_000n, 9_950_000n]);
  // Config layout: entry_fee_bps right after admin, attestor, 7 guardians and 4 bytes.
  const disc = ledgerIdl.accounts.find((a: any) => a.name === "Config")!.discriminator;
  const data = new Uint8Array(400);
  data.set(disc, 0);
  new DataView(data.buffer).setUint16(300, 30, true);
  expect(decodeEntryFeeBps(data)).toBe(30);
});
