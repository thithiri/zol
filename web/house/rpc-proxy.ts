// The pages' Solana RPC (`/solana`), served by the house: only the JSON-RPC methods the pages
// use, over HTTP POST, rate-limited per client address, then passed to a dedicated RPC whose
// URL (and API key) stays on the server. Without the filter, anyone could spend the RPC
// provider's credits through the site.

/** What the pages and the house's own client call (web/src, web/house), plus a few cheap reads. */
export const RPC_METHODS = new Set([
  "getAccountInfo",
  "getMultipleAccounts",
  "getLatestBlockhash",
  "getBalance",
  "sendTransaction",
  "getSignatureStatuses",
  "getTransaction",
  "getBlockHeight",
  "getSlot",
  "getHealth",
  "getVersion",
  "getMinimumBalanceForRentExemption",
  "getFeeForMessage",
]);
export const MAX_BODY = 128 * 1024;
export const MAX_BATCH = 20;

/** The number of calls in a JSON-RPC request body, or why it is refused. */
export function rpcCalls(body: unknown): number | string {
  const calls = Array.isArray(body) ? body : [body];
  if (calls.length === 0 || calls.length > MAX_BATCH) return `a batch holds 1 to ${MAX_BATCH} calls`;
  for (const c of calls) {
    const method = (c as { method?: unknown } | null)?.method;
    if (typeof method !== "string") return "each call needs a method";
    if (!RPC_METHODS.has(method)) return `method ${method} is not served here`;
  }
  return calls.length;
}

/** A token bucket per client address: `rate` calls a second, up to `burst` at once. */
export class RateGate {
  private buckets = new Map<string, { tokens: number; t: number }>();
  constructor(
    private rate = 10,
    private burst = 60,
    private now: () => number = Date.now,
  ) {}

  take(ip: string, n: number): boolean {
    const t = this.now();
    const b = this.buckets.get(ip) ?? { tokens: this.burst, t };
    b.tokens = Math.min(this.burst, b.tokens + ((t - b.t) / 1000) * this.rate);
    b.t = t;
    const ok = b.tokens >= n;
    if (ok) b.tokens -= n;
    this.buckets.set(ip, b);
    // Forget addresses idle long enough to be full again.
    if (this.buckets.size > 10_000) for (const [k, v] of this.buckets) if (t - v.t > (this.burst / this.rate) * 1000) this.buckets.delete(k);
    return ok;
  }
}
