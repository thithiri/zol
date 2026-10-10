# Phase 3: the gateway

Date: 2026-10-10. Status: **done for a single-host cluster.** Every task in plan Phase 3 is implemented and tested. Splitting the operators across machines and putting the system on testnet are listed under gaps.

The Zcash half has its own write-up: [phase3-zol-zcash.md](phase3-zol-zcash.md).

## Architecture

Each gateway operator is two processes:

```text
Zakura (own node; was Zebra 6.4.2 when built)
   │ JSON-RPC
   ▼
zol-zcashd   (crates/zol-zcash, own workspace)
   - Ironwood scanner and event log
   - refund and genesis builders
   - Pallas FROST spend shares
   │ HTTP JSON (crates/zol-types)
   ▼
zol-gateway  (gateway/, root workspace)
   - decisions
   - Ed25519 FROST attestor
   - reconciliation, guardian, halt
   - relayer to the Solana ledger (JSON-RPC)
```

**Why two processes.** pczt 0.9.3 depends on `zcash_transparent`, which pins `bip32 =0.6.0-pre.1`, which needs `sha2 =0.11.0-pre.4`. anchor-lang 1.2.1 needs `sha2 ^0.11`. Cargo allows one 0.11 per build, so no single binary can link both. Avoiding that would mean patching a crypto dependency. The split also keeps the Zcash spend shares out of the process that talks to Solana. The contract between the two halves is `crates/zol-types`.

**The cluster.** In this version, the operators' gateway nodes run in one `zol-gateway` process, and each has its own `zol-zcashd` and its own store. Every node decides from its own scan, and FROST rounds run in-process. Refunds are signed by the coordinator's `zol-zcashd`, which holds a threshold of Pallas shares in cluster mode.

## Plan tasks

| Task | Status | Where |
|---|---|---|
| 3.1 Key material | Done | Per-app vault keys: `zol-zcash::keys`, ZIP 2005 with `use_qsk`, key derivation v1 with network and version bytes. Gateway secrets (deposit-ID and payout-commitment keys), one per app from a master secret: `gateway::config::AppSecrets` |
| 3.2 Scanner | Done | `zol-zcash::scan`: Ironwood trial decryption in both scopes, nullifier tracking, witnesses, reorg rewind, snapshots. `zol-zcashd` serves it as a persistent, cursor-based event log |
| 3.3 Confirmation policy | Done | `gateway::policy`: depths 30 / 300 / 300 in 25 s blocks; blocks before NU7 count three times, so 10 / 100 / 100 before activation (`DepthRule`, docs/ops/nu7.md). Early credits bounded by a per-deposit cap and by the remaining float, which is computed from the operator's own scan (`reconcile::compute_reserve`; the design's worked example is a unit test) |
| 3.4 Memo validation | Done | `gateway::decide`: credit, void (six reasons) or hold. Accepts binary and `ZOL1:` memos. A memo the page registers off-chain (`Node::register_intent`) is used when a wallet dropped the on-chain memo; the note's session receiver ties it back |
| 3.5 Attestor | Done | `gateway::attestor`: FROST(Ed25519) with DKG; the aggregate signature is plain Ed25519, verified by the ledger's precompile. An operator signs only its own view, writes the decision to its log (`synchronous=FULL`) before releasing a share, and refuses a credit/void conflict. The coordinator replaces operators that refuse |
| 3.6 Relayer | Done | `gateway::rpc::RpcLedger`: Solana JSON-RPC; reads at finalized; deposits listed with `getProgramAccounts` filters (the ledger's `Deposit` now stores its ID) |
| 3.7 Reconciliation | Done | `gateway::reconcile`: index coverage, note existence, amount, recomputed digest, credited total against the operator's own notes, early credits within the float. Any mismatch → `freeze_app` with the operator's guardian key |
| 3.8 Refunds | Done | `zol-zcash::refund` builds and signs with FROST: one spend, the payout, and one zero-value padding output added by the builder itself, with fixed OVKs. The gateway's service asks the coordinator's `zol-zcashd` for refunds of voided deposits; repeat requests for the same note return the same transaction |
| 3.9 Genesis head | Done | `zol-zcash::genesis` (dev: `POST /dev/genesis`); `node::check_genesis`. Guardians cancel an activation unless the head is in their scan, internal scope, with a genesis memo for the app, at full depth |
| Global halt (v0.3.1) | Done | Deep reorg, rollback of a full-depth credit, reserve shortfall, operators' nodes disagreeing at early depth, tip stall. Sticky; a halted node signs nothing |

## Tests

| Suite | What it covers |
|---|---|
| `gateway` unit (13) | Store, decision log, policy and the design's float example, decisions, FROST (DKG, plain-Ed25519 check, refusals, single-use nonces), receiver check |
| `gateway/tests/gateway.rs` (12) | Against the real ledger on LiteSVM with a scripted chain: early versus full-depth credit within the float; voids and refunds due; a note seen by one operator is not signed; forged credit freezes the app; misbound credit freezes the app; memo dropped by the wallet but credited from the registered memo; early-credit rollback absorbed or halting; deep reorg halts; tip stall halts; operators on different forks halt; a signed decision is never reversed; a missing genesis head cancels activation |
| `gateway/tests/rpc_validator.rs` (ignored, about 80 s) | Real `solana-test-validator`: credit, void, reconciliation and a guardian freeze over JSON-RPC |
| `gateway/tests/regtest_e2e.rs` (ignored, about 12 s) | **Across processes**: Zebra regtest, three `zol-zcashd`, the gateway cluster, the ledger on LiteSVM. A shallow genesis head cancels activation, then activation at full depth; a `ZOL1:` deposit to a session receiver is credited; an expired memo is voided; a 2-of-3 Pallas FROST refund is broadcast; another operator's scan sees the refund spend; everything reconciles |
| `crates/zol-zcash` | 15 offline tests and 4 regtest tests (deposit, genesis and refund; reorg; fixture; daemon flow with restart) |

Run commands are in the README.

## Gaps

- **Multi-host operators.** FROST rounds (Ed25519 attestor and Pallas spend), chain reports and refund signing run in one process. A deployment where each operator runs alone needs an authenticated transport between operators. `zol-zcashd` in single-share "operator" mode answers `/refund` with 501 until then.
- **Not run on testnet or devnet.** There are no testnet ZEC, and the devnet faucet was refusing airdrops.
- **ZIP 2005 key derivation has no reference vectors.** It was implemented from the ZIP text; cross-check it against librustzcash before real funds, because a mistake changes every vault address.
- **Refunds to a vault address.** If a deposit memo names one of the vault's own receivers as its payout address, the void is recorded but `zol-zcashd` refuses the refund ("refund receiver is a vault address"), so the deposit stays unrefunded. The gateway can't recognise vault receivers itself; add an operator procedure or a `zol-zcashd` check at decision time.
- **Scanner throughput.** Trial decryption is not batched, sync fetches one block per RPC call, and snapshots rewrite the whole state. This is fine for regtest and testnet, not yet for mainnet volume.
- **Refunds carry an empty memo.** There is no refund memo format yet.
- **Phase 4 pieces** (head-chain walker, payout builder and verifier, `mark_paid` after payout depth) are not started. The reserve currently takes the head as the latest unspent head note, rather than walking the chain from the genesis commitment.
