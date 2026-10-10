# ZOL

Real-time apps for shielded ZEC: the ZEC stays in per-app Zcash vaults, a Solana
ledger records who is owed what, and sessions run in MagicBlock ephemeral rollups.
Design: [docs/design-v0.3.md](docs/design-v0.3.md). Plan: [docs/plan-v0.3.md](docs/plan-v0.3.md).

Status: Phases 0–4 of the plan, and the flagship app (Phase 5.3). Spike findings: [docs/spikes/](docs/spikes/). Gateway: [phase3-gateway.md](docs/phase3-gateway.md), [phase3-zol-zcash.md](docs/phase3-zol-zcash.md). Payouts: [phase4-gateway.md](docs/phase4-gateway.md), [phase4-zol-zcash.md](docs/phase4-zol-zcash.md).

## Layout

| Path | What |
|---|---|
| `crates/zol-wire` | `no_std` wire formats: deposit memo v2, head and top-up memos, credit/void/paid digests, settlement record digests, keyed deposit IDs and payout commitments. Golden vectors in `vectors/v0.3.json`; TypeScript twin in `ts/` |
| `programs/zol-ledger` | Anchor 1.2.1 kernel ledger, with MagicBlock delegation via ephemeral-rollups-sdk 0.17.3 |
| `crates/zol-types` | Contract between the gateway's two processes: chain events, the `zol-zcashd` JSON API, the session-receiver index |
| `crates/zol-zcash` | Zcash side (own workspace): per-app vault keys, Zcash node RPC (Zakura), Ironwood scanner, refund and genesis builders, Pallas FROST; the `zol-zcashd` daemon |
| `gateway` | Solana side: decisions, Ed25519 FROST attestor, reconciliation, guardians, halt, RPC relayer; the `zol-gateway` binary |
| `programs/zol-test-app` | Test-only app that forwards ledger calls signed with its `zol-app` address |
| `programs/zol-coinflip` | Flagship app: two-player coin flip on a ledger pot with MagicBlock VRF ([phase5-coinflip.md](docs/phase5-coinflip.md)) |
| `web` | The coin flip page: session key, payout address (UA decoding), signed memo, ZIP 321 link and QR, table actions, exit |
| `tests` | LiteSVM harness and test suites: flow, lifecycle, adversarial, invariant fuzzing |
| `infra/zakura` | Dev and test Zcash node: a Zakura regtest pair (Docker), NU7 or pre-NU7 configs, scripts ([zcash-node.md](docs/ops/zcash-node.md)) |
| `spikes/` | Phase 0 throwaway code (standalone workspaces) |
| `idl/zol_ledger.json` | Generated IDL |

## Build and test

```bash
scripts/test.sh fast
```

```bash
scripts/test.sh regtest
```

```bash
scripts/test.sh full
```

| Tier | What it runs | Time (warm, this machine) |
|---|---|---|
| `fast` | Unit, LiteSVM ledger and coin-flip, zol-zcash offline, web and wire tests; no external services | About 42 s |
| `regtest` | `fast` + every Zakura-backed suite (zol-zcash regtest, gateway cross-process and multi-host), in five parallel lanes, each on its own fresh Zakura pair | About 4.5 min (259 s warm; was 517 s run one after another) |
| `full` | `regtest` + the 2,000-case invariant fuzz + the local Zcash-leg end to end (MagicBlock stack, VRF, three gateways) | Not timed yet; the Zcash-leg run alone took about 8 min |

The tiers use `cargo-nextest` (parallel test processes; config in `.config/nextest.toml`) and `sccache` (`.cargo/config.toml`, shared by both workspaces). Dependencies build optimised in dev, so LiteSVM tests run in seconds. Keep `target/` warm: a cold build adds several minutes per workspace. The invariant fuzz runs 64 cases by default; set `PROPTEST_CASES` for more.

The individual commands below still work.


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
cd web && npm install && npm test
```

```bash
~/.avm/bin/anchor-1.2.1 idl build -p zol-ledger -o idl/zol_ledger.json
```

The Zcash side is a separate workspace on the NU7 pre-release Zcash crates ([docs/ops/nu7.md](docs/ops/nu7.md)). It was split off because pczt 0.9.3 and anchor-lang 1.2 needed incompatible `sha2` releases.

```bash
cd crates/zol-zcash && cargo test && cargo build --release --bin zol-zcashd
```

Regtest and validator tests are ignored by default. Start the Zakura regtest pair with `infra/zakura/scripts/up.sh pair` (NU7 active at height 1; `ZOL_NODE_CONFIG=pre-nu7` for an NU6.3 chain), then:

```bash
cargo test -p zol-gateway --test regtest_e2e -- --ignored --nocapture --test-threads=1
```

```bash
cargo test -p zol-gateway --test rpc_validator -- --ignored --nocapture
```

```bash
cd crates/zol-zcash && cargo test --release -- --ignored --test-threads=1
```

`tests/fixtures/dlp.so` is MagicBlock's delegation program (`DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`), dumped from devnet with `solana program dump`. `delegate_pot` runs the real delegation CPI against it. A return from the rollup is simulated by restoring the pot's ledger ownership with the data a validator would commit, including faulty data. The real rollup path (`move_seat` and `close_pot` in an ephemeral validator) is exercised by `spikes/magicblock/ledger-e2e` on MagicBlock's local stack; see `docs/spikes/0.9-magicblock.md`.

## App quickstart

An app is an Anchor program plus a web page; it holds no ZEC and no keys. `programs/zol-coinflip` and `web/` are the worked example; [docs/phase5-coinflip.md](docs/phase5-coinflip.md) has the full walkthrough and how to run it locally end to end.

1. **Sign as the app.** Every ledger call that needs the app's consent is a CPI signed with `find_program_address(["zol-app"], app_program)`, via `invoke_signed` (`CpiContext::new_with_signer(zol_ledger::ID, …, &[&[b"zol-app", &[bump]]])`). Depend on `zol-ledger` with `features = ["cpi"]`.
2. **Register.** Add an instruction that CPIs `register_app` (anyone pays); the ledger admin activates the app after the vault and genesis head exist, and it becomes usable after the activation delay.
3. **Tables.**
   - `open_pot(pot_id, open_deadline)`.
   - `buy_in(amount)`: the player's **session key** signs alongside the app.
   - `delegate_pot(expected, session_deadline, validator)`: `expected` is the receipt's seats and buy-ins, read from the `PotReceipt`, never from app state. `validator` must be the rollup's identity, or the pot never reaches it.
   - In the rollup: `move_seat(from, to, amount)` by seat index, then `close_pot`.
   - `cash_out` is permissionless.
   - Stakes: the coin flip accepts only one deposit's net value (one of the ledger's `Config.params.denominations` less its entry fee). `close_table` returns a finished or refunded table's rent to its creator, and `rescue_table` brings back a table stuck in the rollup once its pot is refunded (devnet delegation program only, for now).
4. **Your own state** can be delegated with the pot (`#[delegate]`) and committed with it (`MagicIntentBundleBuilder::commit_and_undelegate`). Keep player keys out of it; the receipt is the source of truth.
5. **Randomness.** `#[vrf]` and `#[vrf_callback]` from `ephemeral-rollups-sdk` (feature `vrf`). The callback is safe only if the queue is pinned: accept MagicBlock's queues only.
6. **The page** creates the session key, decodes the player's payout UA to the 43-byte receiver, builds and signs memo v2 (`crates/zol-wire/ts/wire.ts`), registers it with a gateway operator (`POST /v1/apps/{app}/intents`; operators gossip intents to each other, and `AppInfo.operators` lists them all for a page that registers with each), and shows the ZIP 321 link for the session's UA. Rollup transactions need no SOL. `spikes/magicblock/zcash-leg/run.sh` runs the whole thing locally: Zakura regtest, three `zol-zcashd`, three `zol-gateway serve`, the validator, the rollup and the VRF oracle.

## Release and dev builds of the ledger

`scripts/build-sbf.sh` builds two ledgers:

- **`target/deploy/`**: the release build. It is the only one to deploy, and its hash (below) is what signers pin.
- **`target/deploy-dev/`**: the `dev-timings` feature, for local stacks. It lowers the activation-delay floor from 3,600 s to 5 s and the refund-grace floor from 3,900 s to 10 s, so local runs don't wait an hour, and it refuses to initialize for mainnet. Use it with `ZOL_DIR=../../target/deploy-dev` and `ZOL_DEV_TIMINGS=1` in `spikes/magicblock`.

`tests/tests/dev_timings.rs` checks both: the dev build allows the short windows, and the release build rejects them.

## Program hash

Signers pin the ledger's code hash (design rule 2). Build `c5acd985b75a2ce02cce0c49ede162932ea6d59dd695c8251192ef453b95f0a5` is the SHA-256 of `target/deploy/zol_ledger.so` from `scripts/build-sbf.sh`. Two clean builds in different directories gave the same hash on this machine. For a deployed program, compare against `solana-verify get-program-hash`, which hashes the program data without trailing padding, and use `solana-verify build` (Docker) for builds that reproduce across machines.

## Ledger notes

- The ledger never calls app code. Apps prove consent by signing with `find_program_address(["zol-app"], app_program)`.
- Attestations: the instruction just before a credit, void or paid instruction must be an Ed25519 precompile check of the attestor key over exactly the expected 32-byte digest. Every offset must point inside the precompile instruction itself.
- The pot is passed untyped to `delegate_pot`, `close_pot`, `cash_out` and `refund_pot`, because while it is out it belongs to the delegation program. `cash_out` accepts it only if it is ledger-owned, has the receipt's seat count, app and pot ID, has no value past the last seat, and sums to the receipt total.
- `process_undelegation` is added by MagicBlock's `#[ephemeral]` macro. It only works with a signer buffer owned by the delegation program at the canonical undelegate-buffer address, so only the delegation program can restore a pot. Whatever it restores is checked by `cash_out`.
- The admin can change parameters within caps fixed in the program, rotate the attestor and pause. It cannot change guardians, unfreeze, or change a vault address once active. Any one guardian freezes an app or cancels an activation during its delay. A threshold of guardians unfreezes or replaces the guardian set.
