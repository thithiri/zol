# ZOL v0.3 Implementation Plan

Source: `design-v0.3.md`, now `design-v0.3.1.md` after the Phase 0 gate.

**Status (2026-10-10):** Phases 0–2 are complete; see [spikes/0.12-gate-review.md](spikes/0.12-gate-review.md) for each task, what is outstanding (testnet broadcast, devnet deployment, the ledger in a real rollup), and what carries into Phases 3–5. Read "Orchard" below as the Ironwood pool (NU6.3).

Original plan: Each phase ends at a gate; don't start the next phase's safety-critical work until the gate passes. Tasks are numbered `phase.task`. A task marked **[gate]** must be finished before the phase can close.

Repo state at plan time: only `docs/` existed.

## Proposed layout

```text
zol/
  crates/zol-wire/        no_std: memo v2, digests, domain tags, head/top-up memo codecs (shared by ledger + gateway)
  programs/zol-ledger/    Anchor program (kernel)
  programs/coinflip/      flagship app
  gateway/                Rust: scanner, attestor, guardian, coordinator, signer
  web/                    coinflip page: session key, ZIP 321 link, status
  tests/                  ledger tests (litesvm/bankrun), e2e harness, regtest reorg suite
  spikes/                 throwaway Phase 0 code
```

---

## Phase 0 — De-risk the unverified facts (spikes)

Goal: answer every "Unverified external fact" in Open decisions that a later phase depends on. Output is a short findings note per spike, plus a v0.3.1 patch to the design where a fact turns out different.

0.1 Scaffold the workspace above: Cargo workspace, Anchor workspace, CI (fmt, clippy, tests).
0.2 Stand up infrastructure: Zebra on testnet and on regtest (for reorg tests), Solana devnet RPC, MagicBlock devnet endpoint.
0.3 **[gate] One-input payout through the real `pczt` pipeline** (also a roadmap prerequisite): spend one vault note, output one payout and one change ("head") note, include the required dummy spend, broadcast on testnet. Single-key first.
0.4 **[gate] Dummy-spend zero value check**: confirm a signer can recompute a dummy action's value commitment with spend value 0 from the fields `pczt` exposes. If it can't, redesign the dummy rule before Phase 3.
0.5 **[gate] Ciphertext reproducibility**: re-encrypt the output from plaintext, memo, recipient and the supplied randomness, then byte-compare with the `pczt` ciphertext.
0.6 Inventory which checks the `pczt` Signer role already performs; list the gaps the gateway must fill.
0.7 **[gate] Rerandomized FROST over Pallas (ZIP 312)**: 2-of-3 trusted-dealer then DKG, signing the 0.3 transaction's spend authorization. Libraries: `frost-rerandomized` / `reddsa` (pin versions).
0.8 **[gate] Per-app key tweak**: derive `ak_app = ak_group + tweak_app·G`, apply the tweak to each share, and confirm Orchard's sign rule on `ak` (the ỹ bit) can always be met, e.g. by retrying the tweak counter. Derive per-app `nk`/`rivk` from a master secret. Write it up as a spec for external review.
0.9 MagicBlock spike: delegate an account, mutate it in the ER, commit and undelegate. Measure commit-to-return latency (sets the refund grace), the fraud-proof/challenge window, whether a SOL-less session key can pay fees in the ER, and how undelegated `Config`/`App` are read inside the ER. Ask MagicBlock about forced undelegation and slashing.
0.10 Wallet survey: which target wallets (Zashi, Ywallet, Zingo…) accept a binary `0xFF` memo through a ZIP 321 link.
0.11 Zebra reorg behaviour on regtest: deep reorg handling and the halt/recovery procedure. Check the zcashd claim from the review.
0.12 Gate review: update Open decisions in the design (v0.3.1) with the findings; fix the numeric parameters that depend on them (grace period, payout depth).

## Phase 1 — Shared wire formats (`zol-wire`)

1.1 Memo v2 encode/decode exactly per the byte table, including padding and length checks.
1.2 Session-signature message: domain tag, network, vault address and every other memo field; sign and verify.
1.3 Credit, void and paid digests with distinct domain tags; field order fixed and documented.
1.4 Keyed hashes: deposit ID (from note commitment) and payout commitment (from address, session key and app). Pick the hash/KDF.
1.5 Specify and codec the head memo (app, position, last paid seq, records digest, network-fees-spent total, top-ups-swept total) and the top-up memo (float or fee pool).
1.6 Golden test vectors shared by Rust (ledger and gateway) and TS (web).

## Phase 2 — Ledger program (Anchor)

2.1 `Config` plus `initialize`/`set_config`/`set_cap`; compile-time caps on fee rates and the fee allowance.
2.2 `App` lifecycle: `register_app` (bond), `activate_app` (vault address + genesis head commitment, write-once, delay), `freeze_app` (one guardian; also cancels a pending activation), `unfreeze_app` (guardian threshold).
2.3 Ed25519 precompile introspection helper (the previous instruction verifies the attestor key over the exact digest).
2.4 `credit_deposit` / `void_deposit`: both create `Deposit` (so only one can exist); credited total, deposit count, entry fee to fee pool; `Balance` creation and the same-payout-commitment rule.
2.5 Pots on Solana: `open_pot` (app PDA signer, open deadline), `buy_in` (session key + app PDA, seat uniqueness, max seats), `leave_pot` (session key only, while `Open`).
2.6 `delegate_pot`: expected players/buy-ins must equal the receipt; freeze the receipt; session deadline; MagicBlock delegate CPI.
2.7 Rollup instructions: `move_seat` (by index) and `close_pot` (commit + undelegate).
2.8 `cash_out` (all five checks; consumes the receipt) and `refund_pot` (exactly the two cases; a returned, valid pot can never be refunded).
2.9 Exit: `request_exit` (min exit), `settle` (≤16 payouts, protocol fee + network fee allowance to fee pool, per-app sequence), `mark_paid` (paid message).
2.10 Freeze semantics across all instructions (what's blocked vs. still allowed while frozen).
2.11 **[gate] Invariant tests**: property tests and fuzzing for the three invariants over random instruction sequences.
2.12 **[gate] Adversarial tests** for every ledger-side row in the v0.2 and v0.3 change tables: player substitution, leave-before-delegate race, refund after valid return, foreign program address, double credit/void, wrong pot PDA.
2.13 Record the program hash and set up a reproducible build (signers pin it).

## Phase 3 — Gateway: scan, attest, guard, refund

3.1 Key material: per-app viewing keys from the master secret (Phase 0.8 spec); gateway secrets storage.
3.2 Scanner on the operator's own Zebra: every app with a vault, all diversified addresses, external and internal scopes; persistent note DB; reorg-aware.
3.3 Confirmation policy engine: early-credit depth, full depth, per-deposit cap, early credits ≤ remaining float, rollback handling, global halt conditions, multi-node agreement check.
3.4 Memo validation (tag, version, signature, expiry, address parse, denomination) → credit / void / hold-as-`Seen`.
3.5 Attestor: FROST Ed25519 (`frost-ed25519`) DKG and signing; durable decision log written before releasing a share; never both credit and void for one ID.
3.6 Relayer: submits credit/void/paid; can bundle the app's own instruction after `credit_deposit`.
3.7 Reconciliation (rule 4) run continuously: every credited `Deposit` 0..count matches a note (amount + recomputed attestation digest); credited total; depth/cap. On mismatch the guardian key calls `freeze_app`.
3.8 Refund builder: spends only the deposited note, pays the memo address less the fee, creates no vault output; only after a finalized `Voided`.
3.9 Genesis head tool plus the guardian's pre-activation check (note exists, full depth, position-0 memo); auto-cancel otherwise.

## Phase 4 — Vault head and payout signing

4.1 Head-chain walker: from the Solana commitment, follow nullifiers; the new head is the single vault-addressed output; halt on two.
4.2 Reserve accounting: `Reserve_after`, `Claims_after`, remaining float, fee pool from the head-memo running totals; reproduce both worked examples from the design as tests.
4.3 Coordinator: select the head, credited deposit notes and full-depth top-ups; build the `pczt` (payouts, new head, zero-value padding to throwaway addresses); prove; sign dummy spends; send the re-randomizer over a confidential channel.
4.4 **[gate] Signer verifier**, rules 1–8 plus the recompute checklist: head, real spends, dummy spends, outputs, note and value commitments, ciphertexts, fee and expiry, proof, ZIP 244 sighash computed locally.
4.5 Rerandomized FROST Pallas signing round; shares to every signer; any signer can aggregate and broadcast.
4.6 Coordinator rotation in a fixed order; expiry-bounded retries that reuse the head and swap other inputs freely.
4.7 Limits: per-payout and per-hour caps, larger quorum plus delay above them, interval for non-paying transitions, padding bounds.
4.8 Consolidation transitions and the fee-shortfall path (top-ups).
4.9 Payout schedule and batching (several records per tx; fixed action counts).
4.10 `mark_paid` after payout depth.
4.11 **[gate] Adversarial signer suite**: a malicious coordinator violating each recompute rule; two quorums paying one settlement (shared nullifier, only one confirms); an early-credit reorg with a pending payout (the replacement draws on float); signer log loss; extra vault-addressed output; a look-alike head note; a wrong ciphertext; a non-zero dummy.

## Phase 5 — Flagship app and end-to-end

5.1 Coin flip program on v0.3 calls: `create_table`, `join`, `start`, `flip` (MagicBlock VRF), `land`, `finish`; dual-signed `buy_in`, expected stakes on delegate, `move_seat` by index.
5.2 Web page: session key generation, memo v2 with signature, ZIP 321 link/QR, deposit/pot/exit status, `request_exit`.
5.3 E2E on Zcash testnet + Solana devnet + MagicBlock devnet: deposit → credit → buy-in → rollup → cash-out → exit → settle → payout → `mark_paid`.
5.4 E2E unhappy paths: invalid memo → void → refund; expired deposit; frozen app; pot timeout refund; validator withholds the pot; Solana stall.
5.5 Regtest reorg suite: early-credit rollback, payout reorg (head restored, paid once), global-halt triggers.
5.6 Rewrite the app quickstart for v0.3.

## Phase 6 — Hardening before mainnet

6.1 Fix every numeric parameter (depths, caps, float, fees, min exit, seats, deadlines, grace, activation delay, expiry bound).
6.2 Key custody: admin (timelock?), guardian threshold, gateway secrets, operator set and threshold.
6.3 Runbooks: freeze recovery, global halt, Zebra reorg, coordinator outage.
6.4 Monitoring: reconciliation drift, head-chain progress, pending payout age, float headroom.
6.5 Proof-of-reserves auditor tool (viewing key vs. Solana liability/unpaid/credited).
6.6 External audits: ledger program, per-app key derivation, signer verifier.
6.7 Testnet soak with multiple operators on separate infrastructure.

Out of scope here (later roadmap): attested-enclave signers, per-app signer sets, wallet-held session keys, macro/presets/relayer, ZK note inclusion, open-app vouchers, ztxo.

## Critical path

0.3 → 0.4 → 0.5 → 0.7 → 0.8 → 4.4 → 4.11 → 5.3. Phases 1–2 and 0.9–0.11 can run in parallel with the Zcash spikes.
