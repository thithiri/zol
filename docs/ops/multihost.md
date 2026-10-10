# Running operators on separate hosts

Each operator runs, on its own host:

- a Zcash node (Zakura; see [zcash-node.md](zcash-node.md));
- `zol-zcashd`, holding this operator's one Pallas spend share, on localhost only;
- `zol-gateway serve`, holding this operator's Ed25519 attestor share, its guardian key and its relayer key, with its own Solana RPC endpoint.

Gateways talk to each other over HTTP. Put peer traffic on a private network (WireGuard) and the page API behind TLS; the gateway has no TLS of its own. [network.md](network.md) has the setup.

## Attestor keys

On every host at about the same time, with the same ceremony label:

```bash
zol-gateway dkg --config serve.json --ceremony testnet-2026-10
```

Each host serves a mailbox on its `listen` address and exchanges the three FROST DKG rounds with the peers in its serve config, over the signed peer channel (`gateway::dkgnet`):

- Round-2 packages are secret. Each one is encrypted to its recipient with ChaCha20-Poly1305 under an X25519 key that the recipient sent in round 1.
- In a last round every host confirms the group key and a hash of all round-1 packages. That catches an operator that sent different packages to different peers.
- Hosts whose ceremony label, threshold, `order` or guardian keys differ refuse each other.

The command writes `attestor_share` and `attestor_pubkeys` from the serve config, and refuses to overwrite them. It prints the group key; every operator must print the same one. Set it as `Config.attestor` on the ledger, and register each operator's guardian key in `Config.guardians`.

`zol-gateway keygen --operators 3 --threshold 2 --out keys/` does the same in one process, for local devnets and tests.

## Run

```bash
zol-gateway serve --config gateway/examples/serve.example.json
```

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

`/v1/apps/{app}`, `/v1/apps/{app}/sessions/{session}`, `POST /v1/apps/{app}/intents` and `/v1/apps/{app}/sessions/{session}/deposits` (`zol_types::page`) are public, with CORS open.

- **Intent gossip.** Each operator decides from its own store. An intent registered with one gateway is passed on to the others over `/peer/intent`; each receiver checks the session signature itself. Retries back off over about a minute and a half. A page that wants certainty can still register with each operator.
- **`AppInfo.operators`** lists every operator's page API base URL in rotation order: this host's `public_url` and each peer's `public_url` (or its `url` if unset). A page can use it to fail over.

## Held deposits

A deposit is held, neither credited nor voided, when:
- it has no memo and no registered intent;
- its memo's refund address doesn't parse;
- its refund address is a vault receiver;
- or zol-zcashd couldn't answer whether it is one.

The depositor has to come forward with the transaction ID and an address to refund to.

```bash
zol-gateway held list --config serve.json
zol-gateway held release --config serve.json --app <program id> --cmx <cmx> --to <unified address>
```

- **`list`** shows each held note's cmx, value, height, transaction ID, the reason, and any release already recorded.
- **`release`** decodes the address with this operator's own zol-zcashd and refuses a vault address. It then records the release in this operator's store only.

**Each operator runs `release` on its own host, with the same address.** The release turns a node's decision into a void. The void reaches the ledger only when a threshold of operators each released the deposit. The refund is signed only by operators that recorded the same address. So one operator alone can't redirect a deposit, and operators that disagree move nothing. A release can be replaced until the refund is broadcast.

Before releasing, check that the person asking can show they made the payment, for example the transaction from their wallet.

## Parameter rule found in testing

The ledger's per-payout `network_fee_allowance` must be at least 5,000 × the smallest padded action size (`payout.action_sizes[0]`). With the default sizes that is 20,000 zat. Otherwise a single-payout transition's ZIP 317 fee exceeds the fee pool, and the planner waits with `FeePool { need, have }`. The test harness uses 10,000 (LiteSVM ledger tests only); the multi-host test uses 20,000.

## Test

```bash
cargo test -p zol-gateway --test multihost -- --ignored --nocapture
```

The test needs the Zakura regtest (`infra/zakura/scripts/up.sh`), a built `zol-zcashd` and `solana-test-validator`. It runs three gateways in one process, but they share nothing: each has its own store, zcashd, Solana RPC client and HTTP server, and they reach each other only through signed HTTP. It covers:

- a deposit whose wallet dropped the memo, credited from an intent registered with one gateway and gossiped to the other two;
- `AppInfo.operators`;
- a voided deposit and its distributed refund;
- authentication: no signature, an unknown key, a replay, a stale timestamp, and a same-second request with a fresh nonce;
- one operator shut down, after which the other two pay out and mark the settlement paid. This takes about 80 s.

## Not yet

- TLS termination is left to a reverse proxy ([network.md](network.md)).
- Intent gossip is best effort, with no anti-entropy. An operator that was down when an intent was registered won't have it, unless the page registers again.
- A refund already mined can be re-proposed in the moment before a coordinator's node has pulled the spend. zcashd refuses it ("note already spent"), so it only adds log noise.
