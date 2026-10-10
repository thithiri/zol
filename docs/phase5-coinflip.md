# Phase 5.3: the flagship app, zol-coinflip

Date: 2026-10-10. The worked example of design v0.3.1, "Apps and developer surface". It is a two-player coin flip on one ledger pot, settled by MagicBlock's VRF. It has three parts:

- a Solana program, `programs/zol-coinflip`;
- LiteSVM tests, `tests/tests/coinflip.rs`;
- a static page, `web/`.

It was run end to end on MagicBlock's local stack with the real oracle. See "What was run" below.

## The program

`zol-coinflip`: Anchor 1.2.1, `ephemeral-rollups-sdk` 0.17.3 with features `anchor` and `vrf`. Program ID `7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo`; the keypair is `keys/zol_coinflip-keypair.json` (gitignored) and `scripts/build-sbf.sh` copies it to `target/deploy`. The IDL is `idl/zol_coinflip.json`.

| Instruction | Runs on | Signers | What it does |
|---|---|---|---|
| `register` | Solana | payer | One-off. CPI `register_app`, signed with the app's `["zol-app"]` address. The admin then activates the app |
| `create_table(pot_id, stake, open_secs, validator)` | Solana | payer | CPI `open_pot` (signed `zol-app`). Creates `Table ["table", pot_id]`, naming the rollup validator, so players see who decides their pot before joining |
| `join` | Solana | **session key** | Refuses a second seat for the same player and a third player. CPI `buy_in(stake)` (signed `zol-app` plus the session key) |
| `start(session_secs)` | Solana | payer (anyone) | Reads the **receipt**, not app state, for who is playing. Needs 2 seats at the stake. Marks the table `Started`, CPI `delegate_pot` with the receipt's stakes and the **table's** validator, then delegates the table to the same validator. Because the validator was fixed at creation, nobody can start someone else's table towards an unreachable validator |
| `flip(caller_seed)` | Rollup | payer (anyone) | Table `Started` → `Flipping`. Requests VRF randomness with callback `land`. Only MagicBlock's ephemeral queues are accepted (`ALLOWED_QUEUES`) |
| `land(randomness)` | Rollup | **VRF scoped identity only** (`#[vrf_callback]`: `PDA(["identity", coinflip], vrf)` must sign) | `Flipping` → `Landed`. Winner seat = `random_u8 & 1`. CPI `move_seat(loser → winner, loser's amount)`, signed `zol-app` |
| `finish` | Rollup | payer (anyone; may hold no SOL) | `Landed` → `Finished`. CPI `close_pot` (pot commit and undelegate), then commit and undelegate the table |

`cash_out`, `request_exit` and `settle` are ledger calls: anyone calls `cash_out` once the pot is back, and the player's session key signs `request_exit`.

**State.**

- `Table { pot, pot_id, stake, validator, state, winner, randomness, bump }`.
- It stores no player keys. The ledger's `PotReceipt` is the source of truth for seats. A player who leaves an open pot through the ledger (`leave_pot`, no app involved) is simply not in the game: the test `a_player_who_leaves_before_start_is_not_in_the_game` checks this.

**Why the oracle queue is pinned.** `#[vrf_callback]` checks only that the VRF program's identity for this app signs, and that holds for a fulfilment from *any* queue. If `flip` accepted any queue, a caller could name a queue served by an oracle they run and choose the winner. `flip` therefore accepts only `DEFAULT_EPHEMERAL_QUEUE` and `DEFAULT_EPHEMERAL_TEST_QUEUE` from the SDK.

**Costs per table** (spike 0.9 numbers). `start` delegates two accounts, the pot and the table, so a session forfeits two delegation deposits, about 0.003 SOL each on current devnet. These are paid by `start`'s payer. Rollup transactions are free, and `flip` and `finish` work with a payer that has no SOL.

## Tests

### LiteSVM: `cargo test -p zol-tests --test coinflip` (12 pass)

These run against the real ledger and the real delegation program (`tests/fixtures/dlp.so`), and MagicBlock's VRF program (`tests/fixtures/vrf.so`, from `@magicblock-labs/ephemeral-validator` 0.14.10 local dumps).

Simulated, because LiteSVM has no rollup:

1. **In the rollup.** After `start`, the pot and the table are handed back to their programs with the same bytes, which is the view the ephemeral validator executes on.
2. **The oracle.** `land` is sent with the scoped VRF identity marked as signer and signature checks switched off for that one transaction. It is exactly the instruction the oracle's fulfilment CPIs. All other transactions are signature-checked.
3. **The magic program.** A no-op program (`fixtures/noop.so`) sits at `Magic111…`, so `finish` runs `close_pot` and the table commit up to the scheduling CPI.
4. **The VRF queue.** The local ephemeral queue is loaded emptied (`fixtures/vrf-ephemeral-test-queue-empty.json`, see "MagicBlock's local VRF" below), owned by the VRF program as the rollup sees it.

| Test | Checks |
|---|---|
| `full_game_create_join_start_flip_land_finish_cash_out` | Every step's state. Both accounts delegated by `start`. Validator pinned in the receipt. Invariants for the coinflip app. Winner credited 1,497,000, loser 497,000 |
| `seat_zero_can_win` | Same, other seat |
| `non_player_cannot_join_with_someone_elses_session_key` | Victim's key named but not signing → `AccountNotSigner`. Own key signing against the victim's balance → refused by the ledger's seeds |
| `third_player_and_double_join_are_refused` | `AlreadySeated`, `TableFull`; nothing moved |
| `table_must_name_a_validator_and_start_uses_it` | `BadValidator` for the default key; the receipt records the table's validator whoever calls `start` |
| `start_needs_two_players_and_runs_once` | `NotFull`; a second `start` fails (the table belongs to the delegation program) |
| `a_player_who_leaves_before_start_is_not_in_the_game` | The receipt decides the seats |
| `land_only_through_the_vrf_callback` | A player's key → `ConstraintAddress`. The right identity not signing → `AccountNotSigner`. The legacy global identity → `ConstraintAddress`. A second (replayed) callback → `BadTableState` |
| `flip_refuses_an_unpinned_oracle_queue` | A real VRF queue that is not pinned, and a random account → `BadQueue` |
| `land_before_flip_and_finish_before_land_are_refused` | State machine order |
| `double_finish_fails` | `BadTableState`; a second `cash_out` fails at the ledger |
| `cash_out_waits_for_the_pot` | `PotNotReturned` while delegated |

The whole `zol-tests` suite still passes. `check_invariants` gained `check_invariants_for(app_program)`.

### Web: `cd web && npm test` (44 pass)

| File | Checks |
|---|---|
| `test/ua.test.ts` (31) | UA decoding against 27 vectors from `web/tools/ua-vectors`: 12 Orchard-only UAs from `zol_zcash::keys::encode_ua`, 12 Orchard + Sapling + P2PKH UAs from `zcash_address` 0.13, and 3 UAs with no Orchard receiver, across mainnet, testnet and regtest. The page's own encoder reproduces `encode_ua` byte for byte. Also: wrong network refused, checksum error, raw hex receiver, F4Jumble round trips |
| `test/memo.test.ts` (5) | The page's memo equals the Rust golden vector (`crates/zol-wire/vectors/v0.3.json`) byte for byte, signature and text form included. The signature binds the network and the vault. ZIP 321 link carries the `ZOL1:` text. Zatoshi→ZEC. One session key per app, persisted |
| `test/gateway.test.ts` (5) | Against `test/mock-gateway.ts`, which implements `zol_types::page` and verifies the memo signature over the vault address. Full deposit flow (intent registered, session UA, link). Intent sent to every operator, with one down reported but not fatal. Deposit polling, wrong-network and non-denomination refusals, error mapping |
| `test/chain.test.ts` (3) | Instruction builders against `idl/zol_coinflip.json` and `idl/zol_ledger.json`: discriminators, account order, signer and writable flags. Table decoder |

The UA decoder (`web/src/ua.ts`) is implemented here: bech32m from `@scure/base` with the 90-character limit lifted, F4Jumble and its inverse with BLAKE2b personalizations from ZIP 316 (`@noble/hashes`), and canonical compactSize TLV parsing. No small JavaScript library with UA support and F4Jumble was found that we could vouch for. The vectors are the check.

`crates/zol-wire/ts/wire.ts` is now import-free: WebCrypto SHA-256, so `creditDigest` is now async, and its own base64url. `web/src/wire.ts` is a symlink to it. `cd crates/zol-wire/ts && bun test` still passes.

## What was run end to end

### The whole app with the Zcash leg: `spikes/magicblock/zcash-leg/run.sh` (passes, about 2 minutes)

Everything is local and real; only the player's wallet and the Zcash miner are scripted.

**Zcash side.**

- Zebra 6.4.2 regtest (`spikes/zebra`, :18232).
- Three `zol-zcashd --dev --dev-share i` (one Pallas share each).
- Vault and genesis head (30,000,000 zatoshi float) from `/vault` and `/dev/genesis`, mined to full depth.

**Gateway side.**

- `zol-gateway keygen` (FROST DKG, 2-of-3).
- Three `zol-gateway serve` talking authenticated HTTP to each other.

**Solana side.**

- `start-local.sh` with the ledger.
- `Config` and an already-active coinflip `App` preloaded as genesis accounts (`coinflip preload`, same approach as `gateway/tests/multihost.rs`). The attestor is the DKG group key, and the vault and genesis head are the daemons'.
- `zol-coinflip` deployed.
- The VRF oracle on the rollup.

**The player** is `web/scripts/zcash-leg.ts`, running the page's own modules (`deposit.ts`, `gateway.ts`, `ua.ts`, `chain.ts`). Results from `spikes/magicblock/logs/zcash-leg.jsonl`:

| Step | Result |
|---|---|
| `GET /v1/apps/{app}` | `active`, `regtest`, vault address equals the daemons' |
| Page: session key, payout UA decoded, memo v2 signed, intent POSTed to **all three** operators | registered at 3/3. Session UA from the gateway, ZIP 321 link 464 chars |
| Wallet pays p0's link with the `ZOL1:` text memo; p1's wallet **drops the memo** (`0xF6`) | `/dev/send` to each session receiver, 1,000,000 zat |
| Mine 10, page polls `/deposits` | Both `credited` about 20 s after payment, p1 through the registered intent alone. Ledger balance 997,000 each (0.3 % entry fee) |
| `create_table` → `join` ×2 (session keys) → `start` → `flip` (rollup, zero-SOL session key) → VRF oracle → `land` | Coin landed seat 0, rollup pot `[1000000, 0]` |
| `finish` (rollup, zero-SOL session key) → pot and table return → `cash_out` | Balances 1,497,000 / 497,000 |
| `request_exit` ×2, `settle` | Payouts 1,472,509 and 475,509 (0.3 % exit fee and the 20,000 network allowance) |
| Mine; the gateways build, verify and sign the payout, then `mark_paid` after payout depth | Settlement 0 `Paid` 47 s after `settle`. `paid_count 1`, `unpaid_total 0`, `credited_total 2,000,000 = fee_pool 51,982 + paid 1,948,018` |

**First attempt: intents must go to every operator.**

- The first run POSTed the intent to one gateway only.
- The memo-dropped deposit then stayed `seen` at that operator and `held` at the other two. Only one operator had the intent, so no threshold could attest it.
- Gateways do not share intents. The page now registers with every operator listed in its settings (`prepareDeposit` takes all page APIs and reports which accepted). The second run passed.

### Program, rollup and VRF without the gateway: `coinflip setup` / `coinflip play`

Real `initialize`, `register` (CPI `register_app`), `activate_app` and the 3600 s activation delay, credits signed by the throwaway local attestor, then the game.

**Rust client** (`logs/coinflip-e2e-play.jsonl`): every step succeeded.

| Step | Result |
|---|---|
| `start` → visible in the rollup | 2 ms |
| `flip` → `land` (VRF oracle) | 111 ms |
| `finish` (zero-SOL payer) → pot and table back on Solana | **266 ms** |
| `finish` → `cash_out` confirmed | 862 ms |
| Winner (seat 1) | 1,497,000 |
| Loser | 497,000 (`match: true`) |
| Settlement payouts | 485,509 and 1,482,509 |
| Conservation | `credited_total 2,000,000 = fee_pool 31,982 + unpaid 1,968,018` |

The `finish` logs show `ScheduleCommit: parent program id: 3qh6…` for the pot (the ledger) and the coinflip program for the table.

**The page's own code** (`web/scripts/e2e-local.ts`, `logs/coinflip-web-e2e.jsonl`):

| Step | Result |
|---|---|
| `flip` paid by a session key holding 0 SOL → `land` | 103 ms |
| `finish` → return | 414 ms |
| `finish` → `cash_out` | 821 ms |
| Balances | 1,497,000 / 497,000 as expected |
| `request_exit` | Sets `exiting` |

### MagicBlock's local VRF out of the box: broken, fixed here

The queue dumps shipped in `@magicblock-labs/ephemeral-validator` 0.14.10 are full of stale requests from another chain:

| Queue | Pending requests |
|---|---|
| Ephemeral `Sc9MJU…` | 53 |
| Base `GKE6d7…` | 48 |

Effects:

- **Old requests never clear.** Their request slots (about 467,000,000) are ahead of any local chain, so the oracle's fulfilments fail with `OracleMustProvideInDifferentSlot` (error 13) forever.
- **New requests don't fit.** Every new request fails with `AccountDataTooSmall` (first `play` run: `logs/coinflip-e2e-play-stale-queue.jsonl`).
- **Purging doesn't help.** The program's permissionless `PurgeExpiredRequests` (ix 9) runs but removes nothing in this version (`web/scripts/purge-vrf-queue.ts`).

The fix:

- `spikes/magicblock/vrf-queues/clean.py` writes empty queues: `item_count 0`, `cursor 16`, index kept (layout from ephemeral-vrf `api/src/state/queue.rs`).
- `start-local.sh` loads them with `VRF_CLEAN=1`.

With that, MagicBlock's own dice example got its callback 314 ms after the request, and the coin flip ran as above. No test-only randomness path was needed, so the program has none.

## Running it locally

### A. Everything, with Zcash

```sh
./scripts/build-sbf.sh
cargo build --release -p zol-gateway --bin zol-gateway
cargo build --release --manifest-path crates/zol-zcash/Cargo.toml --bin zol-zcashd
spikes/zebra/scripts/up.sh pair                          # Zebra 6.4.2 regtest on :18232
cd spikes/magicblock && npm install && (cd ledger-e2e && cargo build --release --bins) && cd ../..
cd web && npm install && cd ..
spikes/magicblock/zcash-leg/run.sh                       # brings everything up, then plays as the page
```

The script leaves the stack running: base :28899, rollup :27799, gateways :18710–18712, daemons :18310–18312.

To play by hand in a browser:

1. `cd web && VITE_GATEWAY=http://127.0.0.1:18710,http://127.0.0.1:18711,http://127.0.0.1:18712 npm run dev`.
2. Pay the shown link with `POST :18310/dev/send` (`to` = the session receiver bytes, `memo` = the link's memo bytes), then mine with `generate` on Zebra.

Run `run.sh` again to reset; it kills the previous run first.

### B. Programs, rollup and VRF only (no Zcash, real `register_app` and `activate_app`)

```sh
cd spikes/magicblock
ZOL=1 ZOL_DIR=../../target/deploy DLP=devnet VRF_CLEAN=1 ./start-local.sh &      # base :28899, ER :27799
solana airdrop 100 -k keys/payer.json -u http://127.0.0.1:28899
solana program deploy ../../target/deploy/zol_coinflip.so --program-id ../../keys/zol_coinflip-keypair.json \
  -k keys/payer.json -u http://127.0.0.1:28899
VRF_ORACLE_SKIP_PREFLIGHT=true RPC_URL=http://127.0.0.1:27799 WEBSOCKET_URL=ws://127.0.0.1:27800 \
  VRF_ORACLE_HTTP_PORT=28181 node_modules/@magicblock-labs/vrf-oracle-darwin-arm64/bin/vrf-oracle &
./target/e2e/release/coinflip setup      # initialize the ledger, register coinflip, activate
# The activation delay is at least 3600 s (MIN_ACTIVATION_DELAY_SECS). solana-test-validator's
# clock cannot be warped (--warp-slot moves the slot, not unix_timestamp), so wait an hour.
./target/e2e/release/coinflip play       # credits two players, the whole game, cash-out, exit, settle
cd ../../web && npx vite-node scripts/e2e-local.ts   # the same game through the page's own code
```

`coinflip credit <session> [zatoshi]` credits any session key with the throwaway local attestor (`[0xA7; 32]`), standing in for the gateway in this setup.

### C. The page alone (mock gateway)

```sh
cd web && npm install
npm run mock            # mock page API on :8787 (APP, NETWORK, VAULT env), or point at zol-gateway
npm run dev             # http://localhost:5173
```

Settings, saved in localStorage, are editable on the page:

- gateway URLs, comma-separated: **every operator**, because each needs the registered intent;
- base and rollup RPC;
- ledger and app program IDs;
- VRF queue;
- validator (empty: the rollup's `getIdentity`);
- fee payer.

**Fee payer.**

- **Empty:** the session key pays Solana fees. On a local validator, use the airdrop button.
- **A dev relayer secret key:** pays Solana fees for `create_table`, `join`, `start`, `cash_out` and `request_exit`. The session key still signs `join` and `request_exit`.
- **Rollup transactions** (`flip`, `finish`) are always paid by the session key, which needs no SOL.

The page flow:

1. **Session key.** Ed25519, created per app on first load and kept in localStorage.
2. **Payout address.** Paste a UA or a 43-byte receiver in hex. It is decoded and checked against the app's network before anything is signed.
3. **Deposit.** Pick a denomination; the page builds memo v2 with expiry = gateway tip + 1000 and signs it. It POSTs the canonical memo to `/v1/apps/{app}/intents` at every operator, gets the session's UA (checking that all operators agree), and shows the ZIP 321 link (`depositLink`, `ZOL1:` text memo) and a QR code.
4. **Polling.** Deposit status is polled every 5 s.
5. **Table.** Create, join, start, flip, finish and cash out. The table's URL hash carries the pot ID, to share with the second player. The table and pot are read from the rollup while they are delegated.
6. **Exit.** Request exit.

## What is simulated, and open issues

- **LiteSVM** simulates the rollup, the oracle's signature and the magic program, as listed above. The local run covers them for real.
- **The Zcash leg** is real, except the player's wallet (`/dev/send` from the dev wallet, with the link's memo bytes or none) and the miner (`generate`). Activation is preloaded in A, as in `gateway/tests/multihost.rs`; B runs the real `register_app` and `activate_app`.
- **Intents are per operator.** The page must know every operator's page API. A deposit whose wallet drops the memo is credited only if a threshold of operators hold its intent. Gossiping intents between gateways, or listing operators in `AppInfo`, would remove that burden from pages.
- **Page API notes** (now fixed by the gateway side): `SessionInfo.receiver_index` is a decimal string, and `AppInfo.state` can be `"unregistered"`. The page treats any state other than `active` as not ready.
- **Session-key custody** (design, "Apps and developer surface"): the page holds the key in localStorage, so the page can sign anything the key can sign. Moving key creation into the wallet is the fix.
- **Liveness.** A table whose VRF never answers stays `Flipping`. Its pot is refunded by the ledger after the session deadline plus grace (`refund_pot`). `finish` cannot run without a result, by design.
- **No rent recovery.** `Table` accounts are not closed after `Finished`. A `close_table` that returns rent to the creator is a small follow-up.
- **The stake is a free input.** Deposits are fixed denominations, but tables can use any stake up to a player's balance.
- **Devnet.** Not run: the devnet faucet refuses airdrops (spike 0.9). On devnet the queue is `DEFAULT_EPHEMERAL_QUEUE` (`5hBR571x…`), already accepted by `flip`.
