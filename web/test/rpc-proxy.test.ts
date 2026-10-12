import { describe, expect, it } from "vitest";
import { MAX_BATCH, RateGate, rpcCalls } from "../house/rpc-proxy";

describe("the /solana filter", () => {
  it("serves the pages' methods, singly and in batches", () => {
    expect(rpcCalls({ jsonrpc: "2.0", id: 1, method: "getAccountInfo", params: [] })).toBe(1);
    expect(rpcCalls([{ method: "getLatestBlockhash" }, { method: "sendTransaction" }])).toBe(2);
  });
  it("refuses other methods, malformed calls and oversized batches", () => {
    expect(rpcCalls({ method: "getProgramAccounts" })).toMatch(/not served/);
    expect(rpcCalls({ method: "requestAirdrop" })).toMatch(/not served/);
    expect(rpcCalls([{ method: "getSlot" }, { method: "getProgramAccounts" }])).toMatch(/not served/);
    expect(rpcCalls({ id: 1 })).toMatch(/needs a method/);
    expect(rpcCalls(null)).toMatch(/needs a method/);
    expect(rpcCalls([])).toMatch(/batch/);
    expect(rpcCalls(Array.from({ length: MAX_BATCH + 1 }, () => ({ method: "getSlot" })))).toMatch(/batch/);
  });
  it("limits each address to its rate, and refills", () => {
    let t = 0;
    const g = new RateGate(10, 20, () => t);
    expect(g.take("a", 20)).toBe(true);
    expect(g.take("a", 1)).toBe(false);
    expect(g.take("b", 1)).toBe(true); // another address has its own bucket
    t += 500; // half a second: 5 tokens back
    expect(g.take("a", 5)).toBe(true);
    expect(g.take("a", 1)).toBe(false);
  });
});
