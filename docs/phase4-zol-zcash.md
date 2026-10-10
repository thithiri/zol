# Phase 4: `zol-zcash` head chain, payouts and distributed FROST

Date: 2026-10-10. Status: **built and tested.** 17 offline tests pass, including 2 new payout suites. 6 regtest tests pass (against Zebra 6.4.2 when built; since rerun on the Zakura 1.6.0 regtest, NU7 and pre-NU7, see [docs/ops/nu7.md](ops/nu7.md)), including 2 new Phase 4 tests that drive three `zol-zcashd` processes holding one share each. Clippy is clean.

The contract is `crates/zol-types` (`api::{HeadChain, PayoutPlan, Proposal, VerifyRequest/Response, SignKind, Commit*/Share*/AggregateRequest, Broadcast, RefundProposal}`). I made no changes to it.

## Library

- **`head::walk(&Scanner, &AppKeys, genesis_cmx) -> HeadWalk { links: Vec<Link>, fault }`** (`Link { note, memo }`, `.to_api() -> HeadLink`; `current()` is `None` on a fault).
  - **Genesis:** the pinned `cmx` must be a scanned Ironwood note of the app, internal scope, with `HeadMemo::genesis(network, app)`.
  - **Next transition:** the transaction that reveals the current head's nullifier. Its outputs that decrypt under the app's keys (any diversifier, both scopes) must be **exactly one**, an Ironwood note with a valid head memo.
  - **Memo rule:** that memo must equal `prev.advance(Transition { records_paid = Δpaid_count, records_digest = memo.records_digest, network_fee = the transaction's Ironwood value balance, fee_topups_swept = Σ this app's fee-pool top-ups it spends })`. This covers:
    - position + 1;
    - the paid count is monotone;
    - `records_digest` is zero exactly when nothing is paid;
    - the running totals match exactly;
    - app and network are unchanged.
  - **Pure Ironwood:** the transition must be pure Ironwood: no transparent or Sapling part, Orchard value balance 0, and an Ironwood value balance of at least 0.
  - **Reorgs:** the walk is recomputed from the scanner on every call, so it follows rewinds. Look-alike heads are never reached.
  - **Faults** (the `HeadChain.fault` string starts with the name; transition faults add `transition N (tx …)`):
    - `genesis_not_found`
    - `genesis_not_internal`
    - `genesis_memo`
    - `head_spent_without_successor`
    - `two_vault_outputs`
    - `head_not_ironwood`
    - `head_memo_invalid`
    - `transition_not_pure_ironwood`
    - `head_memo_advance`
- **The scanner now records, per transaction that touched a watched note:** its value balances, and whether it has transparent or Sapling parts (`tx_record`, `notes_in_tx`, `spends_in_tx`).
  - Snapshot version is 2 and daemon `state.bin` version is 2, so **delete Phase 3 state dirs**.
- **`payout::propose(params, keys, &Scanner, &PayoutPlan, &Tamper) -> (Proposal, Pczt)`.** With the default `Tamper` this is the honest coordinator; the knobs exist for adversarial tests.
- **`payout::verify(keys, &Scanner, &PayoutPlan, pczt_bytes, pending) -> VerifyResponse`.**
- **`payout::vault_spends(keys, &Pczt) -> Vec<(action, alpha)>`.**
- **`payout::FAILURES`**: the list of failure names.
- **`refund::check_pczt(pczt, keys, deposit_note, to, height)`**: the refund rules on a bare PCZT.

### Builder (`/payout/propose`)

- **Inputs:**
  - always the current head, which must have at least `full_depth` confirmations;
  - then candidates from `eligible`: unspent credited notes (`plan.credited`), and unspent top-ups (`TopUpMemo` for this app and network) at `full_depth`. Credited deposits come first (oldest first), then float top-ups, then fee top-ups.
  - Candidates are added until `inputs ≥ payouts + fee` and `fee ≤ pool left`, at most `max_inputs` besides the head.
  - A consolidation (empty `records`) sweeps up to `max_inputs` candidates.
- **Outputs, in this order:**
  1. The payout lines, with the receiver's external OVK and the **ZIP 302 empty memo (0xF6 then zeros)**.
  2. The new head: vault internal address index 0, internal OVK, memo = `head.advance(...)`.
     - `records_digest` = `records_digest(app, first_seq, [record_digest(app, seq, payouts)…])` with zol-wire's SHA-256 `Hasher`, or zero for a consolidation.
     - `network_fee` = fee.
     - `fee_topups_swept` = the fee-pool top-ups spent.
  3. Zero-valued padding to fresh throwaway receivers, with the padding OVK.
- **Padding:** `max(action_target, spends, payouts + 1) − (payouts + 1)` outputs. The builder therefore never adds dummy outputs; it only adds dummy spends, which IoFinalizer signs before dropping their keys.
- **Fee and value:**
  - fee = 5,000 × max(actions, 2);
  - pool left = `fee_pool_accrued + head.fee_topups_swept − head.network_fees_spent`, plus the fee top-ups swept by this transition;
  - new head = inputs − payouts − fee.
- **Expiry and output:** expiry = tip + 1 + `expiry_delta` (≤ 100). The transaction is IoFinalized and proven. `Proposal.sighash` is the txid.

### Verifier (`/payout/verify`, and inside `/sign/commit`)

Every check runs against the operator's own plan and own scan. All failures are reported, each by one name. `ok` means `failures` is empty.

| Name | Meaning |
|---|---|
| `pczt_parse`, `sighash` | Bytes do not parse / sighash cannot be computed |
| `plan_app_mismatch` | The plan is for another app |
| `head_fault` | The head walk has a fault (or no genesis) |
| `head_pending` | This daemon aggregated a different, live transition for this head |
| `head_depth` | Head confirmations < `head_depth` (default 1; `full_depth` applies to top-ups) |
| `records_not_next` | Records are not consecutive from the head's `paid_count` |
| `payout_receiver_invalid` | A line's `to` is not a 43-byte Orchard-protocol receiver, or its amount is 0 |
| `expiry` | Not in (tip, tip + 1 + `expiry_delta`], or `expiry_delta` > 100 |
| `anchor_unknown` | The anchor is not a tree root of the last 100 scanned blocks |
| `dummy_key_present` | A leftover `dummy_sk` |
| `redacted_field` | Missing `rcv`, `alpha` or output plaintext |
| `user_address` | `user_address` is set |
| `head_not_spent` | No real spend is the current head (rule 3) |
| `real_spend_unknown_note` | `rk = ak_app + [α]G`, but the nullifier is no note of this app (e.g. another app's head with a shifted α, spike 0.8) |
| `spend_not_allowed` | A real spend of this app's note that is not the head, not credited and not a full-depth top-up |
| `duplicate_spend` | The same nullifier twice |
| `too_many_inputs` | More than `1 + max_inputs` real spends |
| `real_spend_cv` | `cv_net ≠ ValueCommit(note value − output value, rcv)` for a real spend |
| `dummy_nonzero` | A non-vault spend whose `cv_net` does not open with spend value 0 |
| `dummy_signature` | A dummy's spend-auth signature is missing or invalid under its `rk` and the sighash |
| `cmx_mismatch` | A note commitment does not follow from the plaintext |
| `vault_outputs_count` | Not exactly one output addressed to the vault (any diversifier, both scopes) |
| `head_scope` | The new head is not internal scope |
| `head_value` | New head ≠ real-spend value − plan payouts − fee |
| `head_memo_advance` | The head memo (decrypted with esk) ≠ the exact advance of the current head |
| `payout_missing` | A plan line (receiver, amount) has no matching output |
| `unexpected_value_output` | A value-carrying non-vault output not in the plan |
| `payout_memo` | A payout decrypts to a memo other than the empty memo |
| `enc_ciphertext`, `out_ciphertext`, `epk` | Re-encryption under the fixed OVK (external payout, internal head, padding OVK for zero outputs) differs, or an output does not decrypt |
| `padding_count` | Zero-valued outputs ≠ the padding the target implies |
| `action_count` | Actions ≠ `max(action_target, real spends, payouts + 1)` |
| `fee_shape` | Value balance ≠ the ZIP 317 fee for the action count |
| `value_balance` | Negative, or ≠ fee |
| `fee_pool` | Fee > pool left (counting fee top-ups swept here) |
| `reserve` | New head + credited and top-up notes left unspent < `claims_after` |
| `proof` | Halo2 proof missing or invalid (PostNu6_3 verifying key, instances from the PCZT) |

The sighash returned is computed by the pczt Signer from the parsed effects. Signers sign exactly that value.

## Daemon endpoints (new)

**`GET /head?app=&genesis=`** → `HeadChain`
- `chain[0]` is the genesis head; the last element is the current head.
- `pending` is the txid of a live transition this daemon aggregated: unexpired, and still spending the current head.
- `fault` names the problem, as listed above.

**`POST /payout/propose`** (`PayoutPlan`) → `Proposal`
- Errors with 422 if the plan cannot be funded within `max_inputs` and the fee pool, if the head is faulty or shallow, or if the records are not next.

**`POST /payout/verify`** (`VerifyRequest`) → `VerifyResponse`

**`POST /refund/propose`** (`RefundRequest`) → `RefundProposal`
- Unsigned and proven, with the same note rules as `/refund`: a known, unspent, external deposit note of the app.
- 409 while a refund of that note this daemon aggregated is pending.

**`POST /sign/commit`** (`CommitRequest`) → `CommitResponse`
- **Verification first:**
  - `Payout { plan }` runs the full verifier against the request's plan and this daemon's scan, and refuses with 422 `verify failed: <names>`.
  - `Refund { app, cmx, to }` runs the refund rules (`refund::check`) against this daemon's own copy of the note, and refuses with 422.
- **Round 1:** a FROST round-1 commitment for **every vault spend**, with this daemon's signing share tweaked to the app.
- **Response:** `identifier` is that share's serialized FROST identifier; `commitments` are `(action, SigningCommitments::serialize())`.
- **Session binding:** the session is bound to the sighash.
  - Re-committing the same session with the same PCZT returns the same commitments.
  - A different PCZT, or a session whose nonces were used, gets 409.

**`POST /sign/share`** (`ShareRequest`) → `ShareResponse`
- Refuses with 409 unless the package contains this signer's own round-1 commitments unchanged.
- For each action it builds the `SigningPackage` itself: message = the session's sighash; randomizer = the action's α from the PCZT it verified.
- Nonces are consumed. A second call gets 409.
- Sessions live in memory; a restart drops them, and `/sign/share` then answers 404.

**`POST /sign/aggregate`** (`AggregateRequest`) → `Broadcast`
- **Which app:** from this daemon's own session if it signed, otherwise from the app whose key the spends' `rk` follow.
- **Steps:** aggregates per action (frost-core verifies each share and the result), inserts the signatures, finalizes and extracts, verifies the raw transaction fully (proof, all signatures, sighash), then broadcasts.
- **Records:** a head spend becomes `pending`; a refund is recorded so `/refund` and `/refund/propose` stay idempotent. Both are persisted.
- Errors with 502 when the node rejects the broadcast.

**Signer selection, and `--dev-share <i>`:**
- The signing share is the daemon's only share in operator mode, or the first share in cluster/dev mode.
- With `--dev --dev-share <i>` (0-based), the daemon keeps every seed-derived key (vault keys, dev wallet, `/dev/*`) but holds **only** dealer share `i`. `/health` then reports `mode: dev-operator` and `signers_held: 1`.
- `POST /refund` (in-process 2-of-3) still works when the process holds at least the threshold; otherwise it returns 501, so use `/refund/propose` with `/sign/*`.

## Tests

| Suite | Result |
|---|---|
| `payout_offline::honest_payout_then_consolidation_advance_the_head` | Payout of record 0 (2 inputs: head plus credited deposit; fee 10,000) verifies and advances the chain: position 1, paid 1, fees spent = fee. A second honest proposal for the same record differs in sighash but spends the same head nullifier, and `head_pending` fires when the first is pending. Re-paying record 0 gives `records_not_next`. A consolidation with `action_target` 4 sweeps the credited deposit and a fee top-up: paid count unchanged, `fee_topups_swept` +1,000,000, fee 20,000 |
| `payout_offline::malicious_coordinator_is_rejected_by_name` | extra payout → `unexpected_value_output`; missing → `payout_missing`; wrong amount → `payout_missing`; wrong receiver → `unexpected_value_output`; non-credited note → `spend_not_allowed`; two vault outputs → `vault_outputs_count`; another key's note (non-zero "dummy") → `dummy_nonzero`; leftover dummy key → `dummy_key_present`; wrong memo advance → `head_memo_advance`; fee above the operator's pool → `fee_pool`; reserve shortfall → `reserve`; wrong sequence → `records_not_next`; tampered payout ciphertext → `enc_ciphertext`; redacted rseed → `redacted_field`; **app B's head with α shifted by t_B − t_A** (rk passes as app A) → `real_spend_unknown_note`, `head_not_spent` |
| `phase4_regtest::phase4_distributed_payouts_consolidation_and_double_spend` (`--ignored`) | Three processes: two `--dev --dev-share 0|1` and one plain operator with share 2. Genesis and deposit via `/dev/*`. Payout 1 is proposed by d0, verified by all three, committed and shared by d0 and d1, and aggregated by d2. A disagreeing plan is refused at commit. txid = sighash. `/head` shows the pending transition, then the advanced chain on every daemon. Payout 2 is signed by d1 and d2 (operator) and aggregated by d0. A consolidation (padded to 4 actions) sweeps a new deposit. **Double payment:** two different proposals for record 2 are both fully signed; A broadcasts; B is rejected by the node's mempool; after mining, the chain shows record 2 paid exactly once by A, and B is not in the chain. Nonce reuse gets 409. A refund through `/refund/propose` and `/sign/*` mines and the `Spent` event appears; the head is untouched |
| `phase4_regtest::phase4_reorged_payout_restores_head_and_pays_once` (`--ignored`) | Partition; a payout is mined on a (head advanced); b mines 4; heal; a reorgs. The daemons' walk is back to the genesis head and the orphaned payout is not resurrected. After its expiry, record 0 is paid again by a different quorum and mined once; a third attempt gets `records_not_next` |
| Earlier suites | All pass: lib 9, offline 3, fixtures/RPC 3, regtest 3, zcashd 1 |

How to run:

```sh
cd crates/zol-zcash && cargo test
../../infra/zakura/scripts/up.sh pair     # Zakura regtest; ZOL_NODE_CONFIG=pre-nu7 for NU6.3
cargo test -- --ignored --nocapture --test-threads=1
```

## Notes for the gateway

- **Pending is local.**
  - `pending` / `head_pending` reflect only transitions **this** daemon aggregated. Other operators' daemons cannot know about a broadcast they did not aggregate.
  - Two quorums racing on one head is still safe, because the nullifier is shared, but liveness depends on the gateway not proposing while a transition is pending.
  - Pending is held until its expiry, even if the transaction was orphaned by a reorg (the node does not return it to the mempool: measured on Zebra 6.4.2 and on Zakura 1.6.0). This is conservative, per the design ("ties up the head until it confirms or its bounded expiry passes"). Use small `expiry_delta` values.
- **`full_depth` also gates the head.** A payout waits until the current head has `full_depth` confirmations, so with `full_depth = N` an app gets one head transition per N blocks. Use 1 for the early-credit style.
- **Inputs:** `max_inputs` counts inputs besides the head. Selection takes the minimum needed for payouts, except for consolidations.
- **Padding and fee:** `action_target` is a floor. The verifier requires exactly `max(target, spends, payouts + 1)` actions, so the fee is predictable from the plan and the inputs.
- **Payout memo:** fixed to the empty memo. If zol-wire later defines a payout memo, both builder and verifier change.
- **Commitments:** `CommitResponse.commitments` and `ShareResponse.shares` are JSON byte arrays (not b64), as the types define. Every signer in the aggregate must cover every vault-spend action.
- **Sessions:** session IDs are per daemon and in memory. Reuse an ID only for the same PCZT.
- **Refund recording:** the distributed refund path records the refund in the aggregating daemon only.
