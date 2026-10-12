# Testing a ZOL app

Four layers, from seconds to hours:

| Layer | What runs | Real | Time |
| --- | --- | --- | --- |
| [LiteSVM harness](#1-the-litesvm-harness) | `tests/` crate | The ledger, your program, MagicBlock's delegation and VRF programs | Seconds |
| [Local stack](#2-the-local-stack) | Zakura regtest, `zol-zcashd`, gateways, MagicBlock's local validator, rollup and VRF oracle | Everything except the Zcash wallet and miner | Minutes |
| [Devnet](#3-devnet-and-the-testnet-beta) | Headless scripts against the testnet beta | Zcash testnet, Solana devnet, MagicBlock devnet | Minutes to hours |
| [Gateway and web tests](#4-gateway-and-web-tests) | `cargo test -p zol-gateway`, `vitest` | Operator code, the page's TypeScript client | Seconds to minutes |

## Test tiers

[`../scripts/test.sh`](../scripts/test.sh) runs everything in the repo in three tiers. It needs
`cargo-nextest` and `sccache`.

```bash
scripts/test.sh fast      # unit, LiteSVM ledger and coin flip, zol-zcash offline, web, wire (~42 s warm)
scripts/test.sh regtest   # fast + every Zakura-backed suite, five parallel lanes (~4.5 min warm)
scripts/test.sh full      # regtest + 2,000-case invariant fuzz + the local Zcash-leg end to end (~16.5 min warm)
```

`KEEP_PAIRS=1 scripts/test.sh regtest` leaves the lanes' Zakura pairs running. Lane logs go to
`target/regtest-lanes/`.

## 1. The LiteSVM harness

### Build the programs first

The harness loads SBF builds from disk, so build before every test run that follows a program
change:

```bash
./scripts/build-sbf.sh
```

[`../scripts/build-sbf.sh`](../scripts/build-sbf.sh) builds `zol-ledger`, `zol-test-app` and
`zol-coinflip` with `cargo build-sbf --tools-version v1.52` (anchor-lang 1.2 needs rustc ≥ 1.89;
the v1.51 default is too old). Override with `SBF_TOOLS_VERSION`.

| Output | What |
| --- | --- |
| `target/deploy/*.so` | Release builds. `Env::bare()` loads these |
| `target/deploy-dev/zol_ledger.so` | The ledger with the `dev-timings` feature: activation-delay floor 5 s (not 3,600), refund-grace floor 10 s (not 3,900); refuses to initialize for mainnet. `Env::bare_from(dir)` loads it. Never deploy it |

A new app must be added to the script's loop (and its keypair to `keys/`), or built the same way:

```bash
(cd programs/<your-app> && CARGO_TARGET_DIR="$PWD/../../target/sbf" cargo build-sbf --tools-version v1.52 --sbf-out-dir ../../target/deploy)
```

### Layout

| Path | What |
| --- | --- |
| [`../tests/src/lib.rs`](../tests/src/lib.rs) | `Env`: the SVM, admin, relayer, attestor key, three guardians, a clock; PDA helpers; every ledger instruction as a method |
| [`../tests/tests/flow.rs`](../tests/tests/flow.rs) | Happy paths: deposit → pot → delegate → return → cash out → exit → settle → paid |
| [`../tests/tests/lifecycle.rs`](../tests/tests/lifecycle.rs) | Config, app registration and activation, guardians, pause, freeze, deposit cap |
| [`../tests/tests/adversarial.rs`](../tests/tests/adversarial.rs) | Bad attestations, apps seating players without consent, validator returning forged pots, settlement rules |
| [`../tests/tests/invariants.rs`](../tests/tests/invariants.rs) | Property test: random honest and dishonest operation sequences never break the ledger invariants |
| [`../tests/tests/coinflip.rs`](../tests/tests/coinflip.rs) | `zol-coinflip` end to end: the pattern to copy for your own app |
| [`../tests/tests/dev_timings.rs`](../tests/tests/dev_timings.rs) | The `dev-timings` build allows short windows; the release build rejects them |
| [`../tests/fixtures/`](../tests/fixtures/) | `dlp.so` (MagicBlock delegation program, dumped from devnet), `vrf.so` (MagicBlock VRF program, from `@magicblock-labs/ephemeral-validator` 0.14.10), `noop.so` (stands in for the magic program), `vrf-ephemeral-test-queue-empty.json` and `vrf-test-queue.json` (VRF queue accounts) |

Run:

```bash
cargo test -p zol-tests                                   # whole suite
cargo test -p zol-tests --test coinflip                   # one file
cargo test -p zol-tests --test coinflip full_game         # one test
PROPTEST_CASES=2000 cargo test -p zol-tests --test invariants --release
```

The invariant fuzz runs 64 cases by default.

### What is real, what is simulated

LiteSVM runs Solana programs, not a rollup. The harness keeps everything real that it can:

| Piece | In the harness |
| --- | --- |
| `zol-ledger`, your program | Real SBF builds |
| Delegation (`delegate_pot`, your `#[delegate]`) | Real CPI into MagicBlock's delegation program (`fixtures/dlp.so`); delegated accounts end up owned by `DELeGG…` |
| Ed25519 attestations | Real precompile checks. `Env.attestor` is a fixed key the config trusts |
| Clock | `env.warp(secs)` / `env.set_time(t)` set the `Clock` sysvar |
| **The rollup** | Simulated: delegated accounts are handed back to their owner programs with the same bytes, which is what the ephemeral validator executes on. Rollup instructions then run as ordinary LiteSVM transactions |
| **The VRF oracle** | Simulated: the callback (`land`) is sent with the scoped VRF identity marked as signer and **signature verification off** for that one transaction. Every other transaction is signature-checked |
| **The magic program** | A no-op program (`fixtures/noop.so`) sits at `Magic111…`, so commit/undelegate CPIs succeed and do nothing |
| **The return** | The accounts already belong to their programs with the committed bytes, which is what undelegation restores. For the test app, `simulate_return` rewrites the pot with whatever a validator commits, faulty data included |

Not covered, so check them on the local stack or devnet: the ephemeral validator itself,
commit and undelegation timing, scheduled tasks (cranks), return actions (Magic Actions)
actually firing, the VRF oracle's real answers, and the price oracle's live updates.

### `Env` in brief

`Env::new()` gives a ledger initialized with `params()`, `zol-test-app` registered, activated,
past its activation delay, with a cap of `u64::MAX / 4`. `Env::bare()` gives the programs loaded and
nothing initialized.

| Takes any app program | Test app (`TEST_APP`) only |
| --- | --- |
| `activate(&p)`, `activate_with(&p, vault, genesis)`, `set_cap(&p, cap)`, `check_invariants_for(&p)`, `Env::credit_digest(&args, &p)`, PDA helpers (`app_pda`, `app_authority`, `pot_pda`, `receipt_pda`, `balance_pda`, `deposit_pda`, `settlement_pda`) | `credit`, `player`, `open_pot`, `buy_in`, `leave_pot`, `delegate`, `expected`, `simulate_return`, `cash_out`, `refund_pot`, `request_exit`, `settle`, `mark_paid`, `balance`, `app_state`, `freeze`, `unfreeze` |

`via_app` routes any ledger instruction through `zol-test-app`, which signs it with its
`zol-app` address. It lets ledger-level tests skip writing an app:

```rust
/// Wraps a ledger instruction so it reaches the ledger through `zol-test-app`,
/// signed by the app's `zol-app` address.
pub fn via_app(ix: Instruction) -> Instruction {
    let authority = app_authority(&TEST_APP);
    let mut accounts = vec![AccountMeta::new_readonly(authority, false), AccountMeta::new_readonly(LEDGER, false)];
    // … the ledger's accounts follow, with the authority's signer flag cleared
}
```

Assertions: `ok!(tx)` unwraps and prints the logs on failure; `assert_zerr!(tx, OverCap)`
checks a `ZolError`; `err_code(&tx)` gives the raw Anchor code for your own errors
(`6000 + YourError::X as u32`).

### Writing a test for your app

The coin-flip suite is the template. The steps:

1. Add your program to [`../tests/Cargo.toml`](../tests/Cargo.toml) with `features = ["no-entrypoint"]`, and to `build-sbf.sh`.
2. `Env::bare()`, then load your `.so` and any fixtures it needs.
3. `initialize`, then **register through your own `register` instruction** (it CPIs `register_app`), `activate`, `warp` past the activation delay, `set_cap` (a new app's cap is 0).
4. Credit players with attested deposits for **your** app.
5. Drive your instructions: open a table (your CPI to `open_pot`), seat players (`buy_in`), start (`delegate_pot`).
6. Hand the delegated accounts back to their programs and run your rollup instructions.
7. Finish, `cash_out`, and call `check_invariants_for(&YOUR_APP)` along the way.

Set-up, from [`coinflip.rs`](../tests/tests/coinflip.rs) (abridged):

```rust
fn new() -> Self {
    let mut env = Env::bare();
    let so = std::fs::read(deploy_dir().join("zol_coinflip.so")).expect("run scripts/build-sbf.sh");
    env.svm.add_program(COINFLIP, &so).unwrap();
    env.svm.add_program(VRF, &std::fs::read(fixture("vrf.so")).unwrap()).unwrap();
    env.svm.add_program(MAGIC_PROGRAM, &std::fs::read(fixture("noop.so")).unwrap()).unwrap();
    // … VRF queue accounts loaded from the JSON fixtures, owned by the VRF program
    ok!(env.initialize());
    let mut cf = Cf { env, flipper: None };
    ok!(cf.register());                              // coinflip's own `register`
    ok!(cf.env.activate(&COINFLIP));
    cf.env.warp(params().activation_delay_secs);
    ok!(cf.env.set_cap(&COINFLIP, u64::MAX / 4));
    cf
}
```

A player with a credited deposit in your app (`Env::credit` is test-app only, so build the
credit yourself):

```rust
fn player(&mut self, amount: u64) -> Keypair {
    let k = Keypair::new();
    self.env.svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
    let args = self.env.credit_args(rand::random(), amount, k.pubkey(), [7; 32], 0);
    let digest = Env::credit_digest(&args, &COINFLIP);
    let ix = ledger_ix(
        zol_ledger::accounts::CreditDeposit {
            relayer: self.env.relayer.pubkey(), config: config_pda(), app: cf_app(),
            deposit: deposit_pda(&args.deposit_id), balance: balance_pda(&cf_app(), &k.pubkey()),
            instructions: IX_SYSVAR, system_program: SYSTEM,
        },
        zol_ledger::instruction::CreditDeposit { args },
    );
    let r = self.relayer();
    let att = self.env.attestor.clone();
    ok!(self.env.send(&[ed25519_ix(&att, &digest), ix], &[&r]));
    k
}
```

`amount` must be one of `params().denominations` (1,000,000 or 10,000,000); the entry fee
(30 bps) leaves 997,000 or 9,970,000 in the balance.

Entering the rollup, after your `start` delegated the pot and your own state:

```rust
/// What the rollup sees after `start`: the same bytes, owned by their programs.
fn enter_rollup(&mut self) {
    for (k, owner) in [(cf_pot(), LEDGER), (cf_table(), COINFLIP)] {
        let mut a = self.env.raw(&k).unwrap();
        assert_eq!(a.owner, DLP, "{k} should be delegated");
        a.owner = owner;
        self.env.svm.set_account(k, a).unwrap();
    }
}
```

A VRF callback (only if you use MagicBlock's VRF) goes through `send_unverified`, which turns
signature checks off for one transaction: see `vrf_callback` in `coinflip.rs`.

The whole game (abridged):

```rust
#[test]
fn full_game_create_join_start_flip_land_finish_cash_out() {
    let mut cf = Cf::new();
    let alice = cf.player(1_000_000);
    let bob = cf.player(1_000_000);
    ok!(cf.create_table(STAKE));                     // CPI open_pot
    ok!(cf.join(&alice));                            // CPI buy_in, session key signs
    ok!(cf.join(&bob));
    ok!(cf.start());                                 // CPI delegate_pot + delegate the table
    assert_eq!(cf.env.raw(&cf_pot()).unwrap().owner, DLP, "pot delegated through the ledger");
    cf.env.check_invariants_for(&COINFLIP);

    cf.enter_rollup();
    ok!(cf.flip());
    ok!(cf.vrf_callback(randomness_for(1)));         // land → move_seat
    ok!(cf.finish());                                // close_pot; magic program is a no-op
    assert!(!cf.pot().delegated, "close_pot marked the pot as returning");

    ok!(cf.cash_out());                              // ledger cash_out, balances in receipt order
    assert_eq!(cf.balance(&bob.pubkey()), 2 * STAKE, "winner credited with the whole pot");
    cf.env.check_invariants_for(&COINFLIP);
}
```

For ledger-level tests that don't need an app, the test app and `simulate_return` are
shorter. From [`flow.rs`](../tests/tests/flow.rs):

```rust
ok!(env.open_pot(POT, 600));
ok!(env.buy_in(POT, &alice, 500_000));
ok!(env.buy_in(POT, &bob, 500_000));
ok!(env.delegate(POT, env.expected(POT), 600));
// Rollup: Alice wins 300_000 of Bob's seat; the validator commits and returns the pot.
env.simulate_return(POT, |p| {
    p.amounts[0] += 300_000;
    p.amounts[1] -= 300_000;
    p.delegated = false;
});
ok!(env.cash_out(POT, &[alice.pubkey(), bob.pubkey()]));
env.check_invariants();
```

`check_invariants_for(&app)` scans every ledger account of the app and asserts:

- balances + live pots == `App.liability`;
- credited deposits sum to `App.credited_total`, and deposit indexes are `0..count`;
- settlement totals, and `unpaid_total`;
- conservation: credited == liability + fee pool + settlements.

Call it after every step that moves value. Also test what anyone can call (close, settle, cash
out) early, late and twice, and that a table always comes home: the
[build-an-app checklist](build-an-app.md#checklist-for-your-own-app) lists the cases.

### Short windows: the `dev-timings` ledger

From [`dev_timings.rs`](../tests/tests/dev_timings.rs):

```rust
let mut env = Env::bare_from(&dev_dir());          // target/deploy-dev
let mut p = params();
p.activation_delay_secs = 5;
p.refund_grace_secs = 10;
ok!(env.initialize_with(&admin, program_data(), p));
```

In LiteSVM you can `warp` past the real hour anyway. The dev build matters on the local stack,
where the clock can't be moved.

## 2. The local stack

Everything here is in the repo, but the scripts are written for `zol-coinflip`. What you must
change for your own app is listed [below](#adapting-the-local-stack-to-your-app).

Prerequisites: Docker (Zakura), solana-cli 3.0 (`solana-test-validator`), Node.js and npm,
Rust, `python3`. The VRF oracle binary the scripts start is
`@magicblock-labs/vrf-oracle-darwin-arm64`, so runs that need the oracle are Apple Silicon
macOS only as written.

### Zcash: Zakura regtest

[`../infra/zakura/`](../infra/zakura/) runs a Zakura 1.6.0 regtest pair in Docker, with
ephemeral state. NU7 is active at height 1. The miner pays the dev wallet of seed `zol-dev`.
Details: [`../docs/ops/zcash-node.md`](../docs/ops/zcash-node.md).

```bash
infra/zakura/scripts/up.sh pair          # a on :18232, b on :18242; `up.sh` for a only
infra/zakura/scripts/mine.sh 5 [a|b]
infra/zakura/scripts/height.sh [a|b]
infra/zakura/scripts/down.sh
ZOL_NODE_CONFIG=pre-nu7 infra/zakura/scripts/up.sh pair   # NU6.3 chain, no NU7
```

`ZOL_NODE_PAIR`, `ZOL_NODE_RPC_PORT` and `ZOL_NODE_B_RPC_PORT` run more pairs side by side.
Partition and reorg scripts: `peer.sh`, `partition.sh`, `reorg.sh`.

### Zcash side: `zol-zcashd --dev`

```bash
cargo build --release --manifest-path crates/zol-zcash/Cargo.toml --bin zol-zcashd
```

On regtest, `--dev --dev-seed zol-dev --dev-share <i>` runs a daemon on deterministic dev
keys, funded by the miner. It serves `/vault?app=`, `/dev/genesis` (makes an app's genesis
head) and `/dev/send` (pays a deposit). See [`../spikes/magicblock/zcash-leg/run.sh`](../spikes/magicblock/zcash-leg/run.sh)
for a working three-daemon config.

### Solana and the rollup: `start-local.sh`

[`../spikes/magicblock/start-local.sh`](../spikes/magicblock/start-local.sh) starts MagicBlock's
local stack (`mb-stack` from `@magicblock-labs/ephemeral-validator` 0.14.10) on non-default
ports:

| Service | HTTP / WS |
| --- | --- |
| `solana-test-validator` (base, delegation program preloaded) | `:28899` / `:28900` |
| `ephemeral-validator` (the rollup) | `:27799` / `:27800` |
| query-filtering-service | `:26699` / `:26700` |

| Env | Effect |
| --- | --- |
| `DLP=npm\|devnet\|mainnet` | Which delegation program binary. `devnet` and `mainnet` need a dump in `dumps/`. Only the devnet binary has the forced-undelegation instructions (26/27) |
| `TICKS=<n>` | `--ticks-per-slot` (default 64, ~400 ms slots; 8 for fast slots) |
| `VRF_CLEAN=1` | Load empty VRF queues from `vrf-queues/`. The npm dumps are full of stale requests, so every new request fails without it |
| `ZOL=1`, `ZOL_DIR=<dir>` | Preload `zol-ledger` and `zol-test-app` as upgradeable programs from `ZOL_DIR` (default `ledger-e2e/artifacts`), upgrade authority `keys/payer.json` |
| `EXTRA_ACCOUNTS="pubkey=file.json …"` | Genesis accounts, e.g. a preloaded ledger `Config` and an active `App` |

One-time setup (`keys/` and `dumps/` are gitignored):

```bash
cd spikes/magicblock
npm install
solana-keygen new --no-bip39-passphrase -s -o keys/payer.json
solana program dump DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh dumps/dlp-devnet.so --url devnet
(cd ledger-e2e && cargo build --release --target-dir ../target/e2e)   # coinflip and devnet binaries
```

### Run B: programs, rollup and VRF, no Zcash

Real `initialize`, `register_app` and `activate_app` on the dev ledger (5 s activation delay).
From [`../docs/phase5-coinflip.md`](../docs/phase5-coinflip.md#running-it-locally):

```bash
cd spikes/magicblock
export ZOL_DEV_TIMINGS=1                     # clients use 5 s activation / 10 s refund grace
TICKS=8 ZOL=1 ZOL_DIR=../../target/deploy-dev DLP=devnet VRF_CLEAN=1 ./start-local.sh &
solana airdrop 100 -k keys/payer.json -u http://127.0.0.1:28899
solana program deploy ../../target/deploy-dev/zol_coinflip.so --program-id ../../keys/zol_coinflip-keypair.json \
  -k keys/payer.json -u http://127.0.0.1:28899
VRF_ORACLE_SKIP_PREFLIGHT=true RPC_URL=http://127.0.0.1:27799 WEBSOCKET_URL=ws://127.0.0.1:27800 \
  VRF_ORACLE_HTTP_PORT=28181 node_modules/@magicblock-labs/vrf-oracle-darwin-arm64/bin/vrf-oracle &
./target/e2e/release/coinflip setup          # initialize the ledger, register coinflip, activate
sleep 8                                      # the dev ledger's activation delay
./target/e2e/release/coinflip play           # credits, game, cash-out, close_table, exit, settle
```

The `coinflip` binary ([`../spikes/magicblock/ledger-e2e/src/bin/coinflip.rs`](../spikes/magicblock/ledger-e2e/src/bin/coinflip.rs))
talks to `:28899` and `:27799` only, signs as admin and relayer with `keys/payer.json`, and
attests deposits with the harness's fixed attestor key.

| Command | What |
| --- | --- |
| `setup` | `initialize` (dev timings with `ZOL_DEV_TIMINGS=1`), coinflip `register`, `activate_app` |
| `play` | Credits, a full game in the rollup with the real VRF oracle, `cash_out`, `close_table`, exit, settle |
| `credit <session> [amount]` | Credits a deposit to any session key, as the attestor would (used by `web/scripts/e2e-local.ts`) |
| `stuck honest\|dead` | A table stuck in the rollup, refunded by the ledger, then rescued (`dead` freezes the rollup; run the stack with `TICKS=8`) |
| `preload <out_dir> <attestor> <vault_hex43> <genesis_hex32> <guardian>…` | Writes `Config` and an active `App` as genesis accounts, skipping the activation delay |
| `settle <session>…`, `settlement <seq>` | Settle exiting balances; print a settlement |

The page's own code against the same stack:

```bash
cd web && npx vite-node scripts/e2e-local.ts
```

With the release ledger (`ZOL_DIR=../../target/deploy`, no `ZOL_DEV_TIMINGS`) activation takes
at least 3,600 s, and `solana-test-validator`'s clock cannot be warped.

### Run A: everything, with Zcash

[`../spikes/magicblock/zcash-leg/run.sh`](../spikes/magicblock/zcash-leg/run.sh) brings up the
whole system and plays one game as the page:

- Zakura regtest (`:18232`) and three `zol-zcashd --dev --dev-share i` (`:18310`–`:18312`);
- the vault from `/vault` and a 30,000,000-zat genesis head from `/dev/genesis`, mined to depth;
- `zol-gateway keygen` (2-of-3), guardian and relayer keys;
- `start-local.sh` with `Config` and an already-active coinflip `App` preloaded (`coinflip preload`), so there is no activation wait;
- `zol-coinflip` deployed and the VRF oracle on the rollup;
- three `zol-gateway serve` (`:18710`–`:18712`);
- [`../web/scripts/zcash-leg.ts`](../web/scripts/zcash-leg.ts) as the player: deposit, game, exit, payout to `Paid`.

```bash
./scripts/build-sbf.sh
cargo build --release -p zol-gateway --bin zol-gateway
cargo build --release --manifest-path crates/zol-zcash/Cargo.toml --bin zol-zcashd
infra/zakura/scripts/up.sh pair
(cd spikes/magicblock && npm install && cd ledger-e2e && cargo build --release --target-dir ../target/e2e)
(cd web && npm install)
spikes/magicblock/zcash-leg/run.sh
```

About 8 minutes, mostly mining to payout depth. The stack stays up afterwards (logs in
`spikes/magicblock/zcash-leg/work/logs/`); running `run.sh` again resets it. `scripts/test.sh
full` runs it and stops the stack.

To play in a browser against it:

```bash
cd web && VITE_GATEWAY=http://127.0.0.1:18710,http://127.0.0.1:18711,http://127.0.0.1:18712 npm run dev
```

Pay the shown link with `POST :18310/dev/send` and mine with `generate` on `:18232`.

For page work without any chain, `npm run mock` serves a mock page API on `:8787` (the pages'
default gateway).

### Adapting the local stack to your app

These pieces are hardwired to `zol-coinflip` (`7G2o74qy…`):

| Piece | Hardwired | To change |
| --- | --- | --- |
| `zcash-leg/run.sh` | `APP=7G2o…`, `coinflip preload`, deploys `zol_coinflip.so`, the player script | Copy it; set `APP`, deploy your `.so`, preload your `App` |
| `ledger-e2e` `coinflip` binary | `APPP = zol_coinflip::ID`; `setup` registers through coinflip's `register` | Copy `setup` / `preload` with your program id and `register` accounts |
| `start-local.sh` `ZOL=1` | Preloads only the ledger and the test app | Deploy your program with `solana program deploy … -u http://127.0.0.1:28899` after the stack is up (as Run B does for coinflip) |
| `web/scripts/zcash-leg.ts`, `e2e-local.ts` | Coin-flip instructions | Your own client |

A local run needs only one `register` from your program; the ledger accepts any number of
apps.

## 3. Devnet and the testnet beta

Headless scripts that run the pages' flows against the live beta
([networks](reference/networks.md)). All run from `web/` with `npx vite-node`.

| Script | What | Needs |
| --- | --- | --- |
| [`examples/quickstart.ts`](../web/examples/quickstart.ts) | `new`, `deposit <utest1…>`, `status`, `flip N`, `withdraw`: a player against the house ([quickstart](quickstart.md)) | A testnet wallet with TAZ. Nothing else: the house pays Solana fees |
| [`house/multi-smoke.ts`](../web/house/multi-smoke.ts) | Deposits (no auto-exit), one session table, `FLIPS` flips, co-signed withdrawal, followed to the Zcash payout | SSH tunnel to op0's `zol-zcashd` (pays the deposits from the funding wallet). Writes the session seed to `keys/multi-smoke-*.json` |
| [`house/house-smoke.ts`](../web/house/house-smoke.ts) | One game as `coinflip.html` plays it: deposit with auto-exit, the house's co-signed create+join, flip, payout | The SSH tunnel. Writes the seed to `keys/house-smoke-*.json` |
| [`house/games-smoke.ts`](../web/house/games-smoke.ts) | `updown` (bets on ZEC/USD) or `auction` (register, seal, reveal; the last player never reveals) against the live house | `PLAYERS`: a JSON array of hex session seeds with ledger balances, e.g. from multi-smoke runs; 2+ for `auction` |
| [`house/bankroll.ts`](../web/house/bankroll.ts) | `-- <count>` deposits credited to the house's session (operators only) | The SSH tunnel, `keys/house-session.hex` |
| [`scripts/testnet-smoke.ts`](../web/scripts/testnet-smoke.ts) | The two-player game from the deposit to the payout broadcast | The SSH tunnel, `keys/page-fee-payer.json` with devnet SOL, the built `devnet` binary |

**The SSH tunnel.** `zol-zcashd`'s `/admin/*` answers loopback peers only, so the scripts that
pay deposits from the funding wallet reach op0's daemon through a tunnel to the default
`http://127.0.0.1:28300`. This needs operator access to the host:

```bash
ssh -N -L 28300:127.0.0.1:18300 <user>@<operator host> &
```

**Environment variables.** Scripts read their settings (`HOST`, `ZCASHD`, `FLIPS`,
`PLAYERS`, `SESSION_FILE`, …) from the environment. `web/vite.config.ts` blanks `process.env`
for browser bundles only; under `vite-node` the real environment is passed through:

```bash
SESSION_FILE=./other-session.json npx vite-node examples/quickstart.ts status
```

**The e2e binaries** in [`../spikes/magicblock/ledger-e2e/src/bin/`](../spikes/magicblock/ledger-e2e/src/bin/):

- `coinflip` is for the **local** stack only (`:28899` / `:27799`); see [Run B](#run-b-programs-rollup-and-vrf-no-zcash).
- `devnet` administers the ledger and the coinflip app on a public cluster (`ZOL_RPC`, default `https://api.devnet.solana.com`). It needs the ledger admin keypair for most commands; see [Deploying](deploying.md#the-devnet-admin-tool).

Devnet observations worth knowing before you test there:

- Public devnet RPC rate-limits (429s) when several clients share an IP; scripts retry.
- Zcash testnet confirms in ~25 s blocks. Credits come at early depth (3 blocks) only within the vault's float, otherwise at full depth (60). Payouts are marked paid at payout depth (60).
- A session key held only in memory strands its balance if the script dies. The scripts write seeds to `keys/` before any money moves; do the same.

## 4. Gateway and web tests

**Gateway** (`gateway/`, needs `scripts/build-sbf.sh` first: the LiteSVM suites load the ledger):

```bash
cargo test -p zol-gateway                     # unit, gateway.rs and payout.rs on LiteSVM with scripted Zcash chains
```

Ignored by default, need Zakura (`infra/zakura/scripts/up.sh pair`), a built `zol-zcashd` and
`solana-test-validator` on `PATH`:

```bash
cargo test -p zol-gateway --test regtest_e2e -- --ignored --nocapture --test-threads=1
cargo test -p zol-gateway --test rpc_validator -- --ignored --nocapture
cargo test -p zol-gateway --test multihost -- --ignored --nocapture
```

`multihost` runs three gateways that share nothing and talk only over signed HTTP, each with
its own `zol-zcashd` and Solana RPC client. It covers intent gossip, a voided deposit's
distributed refund, peer authentication, and payouts with one operator down. Env:
`ZOL_NODE_URL` (default `http://127.0.0.1:18232/`), `ZOL_DEV_SEED` (`zol-dev`),
`ZOL_ZCASHD_BIN`.

**Zcash side** (its own workspace):

```bash
cd crates/zol-zcash && cargo test                                         # offline
cd crates/zol-zcash && cargo test --release -- --ignored --test-threads=1 # regtest, needs Zakura
```

**Web** (the page's TypeScript client: UA decoding, memos, gateway client against
`test/mock-gateway.ts`, instruction builders against the IDLs, sessions, passkeys, the house):

```bash
cd web && npm install && npx vitest run
cd crates/zol-wire/ts && bun test             # wire-format vectors shared with Rust
```
