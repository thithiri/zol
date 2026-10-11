// The house's guards: it co-signs only the exact transaction it built, signed by the
// player; "fund me" is limited per session, per address and per day.
import { describe, expect, it } from "vitest";
import { Keypair, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import { checkSigned, House, HouseError, MemoryStore, type HouseConfig } from "../house/house";

const feePayer = Keypair.generate();
const player = Keypair.generate();

function built() {
  const tx = new Transaction().add(
    SystemProgram.transfer({ fromPubkey: feePayer.publicKey, toPubkey: Keypair.generate().publicKey, lamports: 1 }),
    { keys: [{ pubkey: player.publicKey, isSigner: true, isWritable: false }], programId: SystemProgram.programId, data: Buffer.alloc(0) },
  );
  tx.feePayer = feePayer.publicKey;
  tx.recentBlockhash = Keypair.generate().publicKey.toBase58();
  return { tx, b64: Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64") };
}
const signedBy = (b64: string, k: Keypair) => {
  const t = Transaction.from(Buffer.from(b64, "base64"));
  t.partialSign(k);
  return Buffer.from(t.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64");
};

describe("checkSigned", () => {
  it("accepts the house's own transaction signed by the player", () => {
    const { b64 } = built();
    const tx = checkSigned(b64, signedBy(b64, player), player.publicKey);
    tx.partialSign(feePayer);
    expect(tx.verifySignatures()).toBe(true);
  });

  it("refuses anything else", () => {
    const { b64 } = built();
    const other = built().b64; // same shape, different accounts and blockhash
    expect(() => checkSigned(b64, signedBy(other, player), player.publicKey)).toThrow(/differs/);
    expect(() => checkSigned(b64, b64, player.publicKey)).toThrow(/missing your session/);
    expect(() => checkSigned(b64, "bm90IGEgdHg=", player.publicKey)).toThrow(HouseError);
    // A forged "player signature": bytes in the right slot that do not verify.
    const t = Transaction.from(Buffer.from(b64, "base64"));
    t.signatures.find((s) => s.publicKey.equals(player.publicKey))!.signature = Buffer.alloc(64, 7);
    const forged = Buffer.from(t.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64");
    expect(() => checkSigned(b64, forged, player.publicKey)).toThrow(/bad signature/);
  });
});

describe("fund me", () => {
  const cfg = {
    baseRpc: "http://127.0.0.1:1", erRpc: "http://127.0.0.1:1", ledger: "3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y",
    app: "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo", vrfQueue: PublicKey.default.toBase58(), validator: PublicKey.default.toBase58(),
    gateway: "http://gw", zcashd: "http://zcashd", zcashNode: "http://node", genesisHead: "00".repeat(32),
    maxOpenGames: 3, flipAfterSecs: 60, fundZat: 1_000_000, fundPerDay: 3, fundPerIpPerDay: 2, playsPerIpPerHour: 10,
  } satisfies HouseConfig;
  const sends: any[] = [];
  const http = (async (url: string, init?: any) => {
    if (url.includes("/sessions/")) {
      const registered = !url.includes("ff".repeat(32));
      return new Response(JSON.stringify({ ua: "utest1session", intent_registered: registered }));
    }
    if (url.endsWith("/admin/send")) {
      sends.push(JSON.parse(init.body));
      return new Response(JSON.stringify({ txid: Array(32).fill(1) }));
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;
  const house = new House(cfg, House.sessionFromSeedHex("11".repeat(32)), feePayer, new MemoryStore(), http);
  const s = (n: number) => n.toString(16).padStart(64, "0");

  it("pays a registered session once, and enforces the limits", async () => {
    const r = await house.fund(s(1), "ip-a", "ZOL1:abc");
    expect(r.value).toBe(1_000_000);
    expect(sends.at(-1)).toEqual({ to: "utest1session", value: 1_000_000, memo: "ZOL1:abc" });
    await expect(house.fund(s(1), "ip-b")).rejects.toThrow(/already funded/);
    await house.fund(s(2), "ip-a");
    await expect(house.fund(s(3), "ip-a")).rejects.toThrow(/your address/);
    await house.fund(s(4), "ip-c");
    await expect(house.fund(s(5), "ip-d")).rejects.toThrow(/daily funding/);
    expect(sends.length).toBe(3);
  });

  it("stop marks the player's live session once, and ignores others", () => {
    const store = new MemoryStore();
    const h = new House(cfg, House.sessionFromSeedHex("33".repeat(32)), feePayer, store, http);
    const g = { id: "aa", player: "P", stake: "99700", state: "in_rollup", created: 0, updated: 0, events: [], rounds: 10 } as any;
    store.put(g);
    store.put({ ...g, id: "bb", player: "Q" });
    expect(h.stop("P")).toEqual({ stopping: true, game: "aa" });
    expect(h.stop("P")).toEqual({ stopping: true, game: "aa" });
    expect(store.get("aa")!.stop).toBe(true);
    expect(store.get("aa")!.events.length).toBe(1);
    expect(store.get("bb")!.stop).toBeUndefined();
    store.put({ ...store.get("aa")!, state: "done" });
    expect(h.stop("P")).toEqual({ stopping: false });
  });

  it("needs a deposit link first, and a well-formed request", async () => {
    const h = new House(cfg, House.sessionFromSeedHex("22".repeat(32)), feePayer, new MemoryStore(), http);
    await expect(h.fund("ff".repeat(32), "ip")).rejects.toThrow(/deposit link first/);
    await expect(h.fund("xyz", "ip")).rejects.toThrow(/32-byte hex/);
    await expect(h.fund(s(9), "ip", "not a memo")).rejects.toThrow(/ZOL1 text/);
  });
});
