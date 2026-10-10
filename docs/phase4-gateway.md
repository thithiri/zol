# Phase 4: the vault head and payouts

Date: 2026-10-10. Status: **done for a single-host cluster.** Payouts work end to end across processes on regtest. The Zcash side is documented in [phase4-zol-zcash.md](phase4-zol-zcash.md), including the builder, the verifier and every failure name.

## How a payout happens

1. **Settle.** Players `request_exit`, and anyone calls `settle`. The ledger writes the app's next settlement record.
2. **Plan.** Every operator's gateway node builds its own `PayoutPlan` (`gateway::payout::plan`). It uses only its own data: finalized ledger reads, its own scan, and the head chain from its own zcashd (`GET /head`, walked by nullifiers from the genesis commitment pinned in `App`). The plan contains:
   - the records that follow the head's paid count, in order;
   - each payout commitment resolved to the payout address in the signed deposit memo behind it (rule 5);
   - the credited notes that may be spent;
   - the fee pool;
   - the claims left after this transition, liability plus unpaid records not paid here (rule 6);
   - the padding target.

   It refuses while the app is not active, while reconciliation does not match (rule 4), while a transition is pending, during a large payout's delay, past the hourly cap, or before the schedule allows (rule 8).
3. **Propose.** The coordinator's zcashd builds and proves the transition (`POST /payout/propose`). It spends the head plus credited deposits and top-ups. It creates the payout lines with a fixed empty memo, then the new head with the exactly advanced head memo, then zero-value padding. The coordinator rotates after a failed attempt.
4. **Verify and sign.** Each operator's own zcashd, holding one Pallas share, runs the full verifier against that operator's own plan before committing (`POST /sign/commit`). It then signs (`/sign/share`), building the signing package itself from the sighash and each spend's `alpha`. Two operators whose plans differ cannot both sign the same proposal. A large payout needs every operator.
5. **Broadcast.** The coordinator's zcashd aggregates, verifies the raw transaction fully, and broadcasts (`/sign/aggregate`). Every node records the transition as pending until it is mined or expires.
6. **Mark paid.** Once each signer's own head chain shows the paying transition at payout depth (100 blocks), a 2-of-3 Ed25519 attestation marks the settlement paid on the ledger, strictly in order. A node halts if a settlement it marked paid is later no longer paid by its head chain, or if its head chain has a fault.

Refunds use the same distributed path: `POST /refund/propose`, then `/sign/*` with `SignKind::Refund`. Each operator commits only if its own view lists the same refund, so single-share operators can refund.

## Plan tasks

| Task | Status | Notes |
|---|---|---|
| 4.1 Head-chain walker | Done | zol-zcash `GET /head`; nine named faults; follows reorgs; look-alike head notes ignored. The gateway halts on a fault |
| 4.2 Reserve accounting | Done | Gateway: `compute_reserve` now uses the walked head. zcashd: `reserve` check against `claims_after`. The design's float example is a unit test |
| 4.3 Coordinator | Done | `POST /payout/propose`; padding by the builder; fixed OVKs; fixed empty payout memo |
| 4.4 Signer verifier | Done | 39 named checks (`payout::FAILURES`), including the nullifier-to-own-note rule against the spike 0.8 cross-app attack |
| 4.5 FROST signing | Done | Distributed rerandomized Pallas FROST across processes, one share per daemon, nonces bound to the sighash and single use |
| 4.6 Rotation and retries | Done | The coordinator rotates per app on failure. An expired attempt is retried against the same head |
| 4.7 Limits | Done | Per-payout cap with delay and an all-operator quorum, a rolling hourly cap, a consolidation interval, and fixed action sizes |
| 4.8 Consolidation and fees | Done | A transition that pays nothing sweeps credited notes and top-ups (zcashd-tested). The fee comes from the fee pool; a shortfall blocks the proposal until a top-up |
| 4.9 Schedule and batching | Done | Up to `max_records_per_tx` records per transition; interval and batch thresholds |
| 4.10 `mark_paid` | Done | At payout depth, from each signer's own head chain; a paid rollback halts |
| 4.11 Adversarial suite | Done | See Tests |

**Head depth.** A plan carries a separate `head_depth`, default 1. The genesis head is already at full depth when guardians accept it. A later head was created by the vault itself, and a reorg that removes it also removes its successor, so each app can move every block. Top-ups still need `full_depth`.

## Tests

| Suite | What it covers |
|---|---|
| `gateway/tests/payout.rs` (6) | Plan pays the next record to memo addresses, and every operator derives the same plan; nothing to pay; pending blocks until expiry; large-payout delay and all-operator quorum; hourly cap; frozen app; `mark_paid` after payout depth, then halt when the paying transition disappears; a head-chain fault halts |
| `crates/zol-zcash/tests/payout_offline.rs` | An honest payout then consolidation. Two honest proposals share the head nullifier. A malicious coordinator's mutations are each rejected by name: extra or missing payout, wrong amount or receiver, non-credited note, two vault outputs, non-zero dummy, leftover dummy key, wrong memo advance, fee above the pool, reserve shortfall, wrong sequence, tampered ciphertext, redacted rseed, and app B's head spent with a shifted `alpha` |
| `crates/zol-zcash/tests/phase4_regtest.rs` (ignored) | Three single-share processes: two payouts by different quorums, a consolidation, a double-payment race (one broadcast, the other rejected by Zebra), nonce reuse refused, a disagreeing plan refused, a distributed refund. A reorged payout restores the head, and the record is paid exactly once afterwards |
| `gateway/tests/regtest_e2e.rs` (ignored) | **Gateway across processes**, three single-share `zol-zcashd`. Phase 3 flow with a distributed refund. Phase 4: two players credited, exit and settle; the quorum proposes, verifies and signs; the head advances on-chain to position 1 with `paid_count` 1; after payout depth the settlement is marked paid on the ledger; everything reconciles |

Run them with the commands in the README. Use a fresh regtest chain (`infra/zakura/scripts/down.sh && up.sh pair`) once the regtest block subsidy has fallen below 0.1 ZEC.

## Gaps

- **Multi-host operators.** FROST rounds are driven by the in-process cluster over each operator's localhost zcashd. Separate hosts need an authenticated transport between gateways. Shares go only to the coordinator; the design's "shares to every signer, so any can aggregate" would come with that transport (`/sign/aggregate` already works on any daemon).
- **Pending state is local** to the daemon that aggregated, plus each gateway node's own record. A reorged-out transition blocks its app until expiry, because the node does not return it to the mempool. Keep `expiry_delta` small (40 by default).
- **Payout memo.** The payout memo is fixed empty. A player's wallet sees an unlabelled shielded payment.
- **Not run on testnet or devnet** (no funds).
- Carried over from Phase 3: ZIP 2005 derivation needs reference vectors; refunds to a vault receiver; scanner throughput.
