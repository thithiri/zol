# Running operators on separate hosts

Each operator runs, on its own host:

- a Zcash node (Zebra or Zakura; see [zcash-node.md](zcash-node.md));
- `zol-zcashd`, holding this operator's one Pallas spend share, on localhost only;
- `zol-gateway serve`, holding this operator's Ed25519 attestor share, its guardian key and its relayer key, with its own Solana RPC endpoint.

Gateways talk to each other over HTTP. Put that traffic behind TLS or a private network such as WireGuard; the gateway has no TLS of its own.

```bash
zol-gateway keygen --operators 3 --threshold 2 --out keys/
```

```bash
zol-gateway serve --config gateway/examples/serve.example.json
```

`keygen` runs the attestor key generation in one process. For production, run its three parts on the three hosts; the code is `attestor::dkg`. Set the printed group key as `Config.attestor` on the ledger, and register each operator's guardian key in `Config.guardians`.

## What each gateway does

**Every poll** (`poll_ms`):

1. Pull its own zcashd's events.
2. Check for a tip stall.
3. Read the head chains.
4. Reconcile and act as guardian.
5. Exchange chain reports with every reachable peer, and halt on disagreement.

**When it's this host's turn to coordinate.** The coordinator is `order[(now / epoch_secs) mod n]`, and it only starts rounds in the first half of its epoch. It runs the rounds over itself and its peers:

- **Attestation:** it asks each peer to commit. A peer commits only if its own view gives the same statement and its log holds no conflicting decision. The coordinator aggregates and relays to the ledger, including `mark_paid` in order.
- **Refunds and payouts:** the coordinator's zcashd proposes. Each peer's gateway derives its own payout plan, or checks its own refund list, and asks its own zcashd to verify and commit. The coordinator's zcashd aggregates and broadcasts. Every peer is told, so it tracks the spend as pending.

**Two coordinators at once are safe.** The ledger admits one `Deposit` account per ID, and two vault transitions share the head's nullifier, so only one can confirm. A coordinator that is down costs one epoch.

## Peer authentication

Every `/peer/*` request carries:

| Header | Content |
|---|---|
| `x-zol-peer` | The sender's guardian public key (hex) |
| `x-zol-ts` | Unix time |
| `x-zol-nonce` | 16 random bytes (hex) |
| `x-zol-sig` | Ed25519 signature |

The signature covers `"zol-peer-v2\n" ‖ path ‖ "\n" ‖ ts ‖ "\n" ‖ nonce ‖ "\n" ‖ hex(sha256(body))`.

A receiver accepts only keys listed in `peers`, timestamps within 60 s, and each (key, nonce) once. Ed25519 signatures are deterministic, so without the nonce two identical requests in the same second would be indistinguishable from a replay. Responses are not signed; their integrity relies on TLS. A tampered commitment or share only breaks liveness, since aggregation verifies every share and every signature.

## Page API

`/v1/apps/{app}`, `/v1/apps/{app}/sessions/{session}`, `POST /v1/apps/{app}/intents` and `/v1/apps/{app}/sessions/{session}/deposits` (`zol_types::page`) are public, with CORS open. A page should register its intent with every operator, since each decides from its own store.

## Parameter rule found in testing

The ledger's per-payout `network_fee_allowance` must be at least 5,000 × the smallest padded action size (`payout.action_sizes[0]`). With the default sizes that is 20,000 zat. Otherwise a single-payout transition's ZIP 317 fee exceeds the fee pool, and the planner waits with `FeePool { need, have }`. The test harness uses 10,000 (LiteSVM ledger tests only); the multi-host test uses 20,000.

## Test

```bash
cargo test -p zol-gateway --test multihost -- --ignored --nocapture
```

The test needs the Zebra regtest pair, a built `zol-zcashd` and `solana-test-validator`. It runs three gateways in one process, but they share nothing: each has its own store, zcashd, Solana RPC client and HTTP server, and they reach each other only through signed HTTP. It covers:

- a deposit whose wallet dropped the memo, credited from the registered intent;
- a voided deposit and its distributed refund;
- authentication: no signature, an unknown key, a replay, a stale timestamp, and a same-second request with a fresh nonce;
- one operator shut down, after which the other two pay out and mark the settlement paid. This takes about 80 s.

## Not yet

- TLS termination and certificate pinning are left to the deployment.
- The attestor key generation runs in one process (`keygen`); the multi-host version, exchanging the three parts over the peer channel, is not wired up.
- A refund already mined can be re-proposed in the moment before a coordinator's node has pulled the spend. zcashd refuses it ("note already spent"), so it only adds log noise.
