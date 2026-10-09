# ZOL

Real-time apps for shielded ZEC: the ZEC stays in per-app Zcash vaults, a Solana
ledger records who is owed what, and sessions run in MagicBlock ephemeral rollups.
Design: [docs/design-v0.3.md](docs/design-v0.3.md). Plan: [docs/plan-v0.3.md](docs/plan-v0.3.md).

Status: Phases 0–2 of the plan. Spike findings are in [docs/spikes/](docs/spikes/).

## Layout

| Path | What |
|---|---|
| `crates/zol-wire` | `no_std` wire formats: deposit memo v2, head and top-up memos, credit/void/paid digests, settlement record digests, keyed deposit IDs and payout commitments. Golden vectors in `vectors/v0.3.json`; TypeScript twin in `ts/` |
| `programs/zol-ledger` | Anchor 1.2.1 kernel ledger, with MagicBlock delegation via ephemeral-rollups-sdk 0.17.3 |
| `programs/zol-test-app` | Test-only app that forwards ledger calls signed with its `zol-app` address |
| `tests` | LiteSVM harness and test suites: flow, lifecycle, adversarial, invariant fuzzing |
| `spikes/` | Phase 0 throwaway code (standalone workspaces) |
| `idl/zol_ledger.json` | Generated IDL |

## Build and test

Requirements: Rust 1.97.1 for the host (pinned in `rust-toolchain.toml`, because LiteSVM 0.18 needs it), solana-cli 3.0 with platform-tools v1.52 (anchor-lang 1.2 needs rustc ≥ 1.89, and the v1.51 default is too old), anchor-cli 1.2.1 for the IDL, and bun for the TypeScript vectors.

```bash
./scripts/build-sbf.sh
```

```bash
cargo test --workspace
```

```bash
PROPTEST_CASES=2000 cargo test -p zol-tests --test invariants --release
```

```bash
cd crates/zol-wire/ts && bun test
```

```bash
~/.avm/bin/anchor-1.2.1 idl build -p zol-ledger -o idl/zol_ledger.json
```

`tests/fixtures/dlp.so` is MagicBlock's delegation program (`DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`), dumped from devnet with `solana program dump`. `delegate_pot` runs the real delegation CPI against it. A return from the rollup is simulated by restoring the pot's ledger ownership with the data a validator would commit, including faulty data. The real rollup path (`move_seat` and `close_pot` in an ephemeral validator) is exercised by `spikes/magicblock/ledger-e2e` on MagicBlock's local stack; see `docs/spikes/0.9-magicblock.md`.

## Program hash

Signers pin the ledger's code hash (design rule 2). Build `0cb9292678bd0d69c92d4dea1bbc7e291fc849173b1f87fc0c2b624f16beaf88` is the SHA-256 of `target/deploy/zol_ledger.so` from `scripts/build-sbf.sh`. Two clean builds in different directories gave the same hash on this machine. For a deployed program, compare against `solana-verify get-program-hash`, which hashes the program data without trailing padding, and use `solana-verify build` (Docker) for builds that reproduce across machines.

## Ledger notes

- The ledger never calls app code. Apps prove consent by signing with `find_program_address(["zol-app"], app_program)`.
- Attestations: the instruction just before a credit, void or paid instruction must be an Ed25519 precompile check of the attestor key over exactly the expected 32-byte digest. Every offset must point inside the precompile instruction itself.
- The pot is passed untyped to `delegate_pot`, `close_pot`, `cash_out` and `refund_pot`, because while it is out it belongs to the delegation program. `cash_out` accepts it only if it is ledger-owned, has the receipt's seat count, app and pot ID, has no value past the last seat, and sums to the receipt total.
- `process_undelegation` is added by MagicBlock's `#[ephemeral]` macro. It only works with a signer buffer owned by the delegation program at the canonical undelegate-buffer address, so only the delegation program can restore a pot. Whatever it restores is checked by `cash_out`.
- The admin can change parameters within caps fixed in the program, rotate the attestor and pause. It cannot change guardians, unfreeze, or change a vault address once active. Any one guardian freezes an app or cancels an activation during its delay. A threshold of guardians unfreezes or replaces the guardian set.
