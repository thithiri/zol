# Draft report for Helius: stale devnet account data at a current context slot

**Where to post:** Helius support (dashboard ticket) or their Discord. Do not include the API
key.

**Title:** Devnet `getAccountInfo` returns account data tens of minutes old with a current `context.slot`

**Body:**

On Oct 11, 2026, on our Helius devnet endpoint, about 40% of `getAccountInfo` calls (encoding
base64, commitment finalized) returned account data that was 70+ minutes old, while the
response's `context.slot` was current. The same accounts read through
`https://api.devnet.solana.com` at the same time returned the up-to-date data. Because the
slot looks current, a client cannot detect the stale answer from the response (we check
`context.slot` against the newest slot seen; it did not help).

The accounts were program-owned PDAs updated every few minutes (a ledger's app and balance
accounts). We switched back to the public devnet RPC. Happy to share account addresses,
timestamps and request IDs if useful.
