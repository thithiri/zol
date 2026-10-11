# ZOL ledger program reference

`zol-ledger` is the Anchor program on Solana that records who is owed what for the ZEC held in each app's Zcash vault. Players deposit shielded ZEC into a per-app vault held by 2-of-3 operators. An attested credit then adds the deposit to a `Balance` on Solana, keyed by the player's session key. Apps are separate Anchor programs that call the ledger by CPI. They move balances into pots, delegate a pot to a MagicBlock rollup, move value between its seats there, and bring the pot back. The pot is then paid out to the players' balances. Players leave through exit requests, which become settlement records that the operators pay on Zcash. The ledger never calls app code. Value enters only through attested credits and leaves only through settlement records.

Source: [`programs/zol-ledger/src/`](../../programs/zol-ledger/src/). IDL: [`idl/zol_ledger.json`](../../idl/zol_ledger.json). The Rust code is the source of truth; this page follows ledger version `0.3.0`.

## Contents

1. [Program ID and calling the ledger from an app](#1-program-id-and-calling-the-ledger-from-an-app)
2. [Accounts](#2-accounts)
3. [Instructions](#3-instructions)
   - [Admin and guardians](#31-admin-and-guardians)
   - [App registration and activation](#32-app-registration-and-activation)
   - [Deposits](#33-deposits)
   - [Pots](#34-pots)
   - [Exits and settlements](#35-exits-and-settlements)
   - [SDK-generated: `process_undelegation`](#36-sdk-generated-process_undelegation)
4. [Events](#4-events)
5. [Errors](#5-errors)
6. [Invariants the ledger guarantees](#6-invariants-the-ledger-guarantees)

---

## 1. Program ID and calling the ledger from an app

| Network | Program ID |
|---|---|
| Solana devnet | `3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y` |

The ID is set with `declare_id!` in [`lib.rs`](../../programs/zol-ledger/src/lib.rs) and is `zol_ledger::ID` in Rust.

### The app authority

Each app acts through one address: the PDA with seed `["zol-app"]` under the app's own program ID. The constant is `zol_ledger::state::seeds::APP_AUTHORITY`. Only the app program can sign for this address, through `invoke_signed`. The ledger stores it as `App.authority` at registration, and copies it into every pot as `Pot.app_authority`.

These instructions require the app authority as a signer: `register_app`, `open_pot`, `buy_in`, `delegate_pot`, `move_seat`, `close_pot`. An app can therefore only call them by CPI from its own program.

### Depending on the crate

Add the ledger as a dependency with the `cpi` feature. This enables `no-entrypoint` and generates `zol_ledger::cpi::*` (one function per instruction) and `zol_ledger::cpi::accounts::*` (one struct per accounts list). From [`programs/zol-coinflip/Cargo.toml`](../../programs/zol-coinflip/Cargo.toml):

```toml
[features]
idl-build = ["anchor-lang/idl-build", "zol-ledger/idl-build"]

[dependencies]
zol-ledger = { path = "../zol-ledger", features = ["cpi"] }
```

Account types, seeds and helpers are public in `zol_ledger::state` (`Config`, `Params`, `App`, `Balance`, `Pot`, `PotReceipt`, `SeatStake`, `PotAction`, `ActionAccount`, `seeds`, `limits`, `bps`). Apps can deserialize ledger accounts with them, for example to read `Pot.amounts` in the rollup.

### CPI example

From [`zol-coinflip`](../../programs/zol-coinflip/src/lib.rs):

```rust
pub const APP_AUTHORITY_SEED: &[u8] = b"zol-app";

zol_ledger::cpi::move_seat(
    CpiContext::new_with_signer(
        zol_ledger::ID,
        zol_ledger::cpi::accounts::MoveSeat {
            authority: a.app_authority.to_account_info(),
            pot: a.pot.to_account_info(),
            app_authority: a.app_authority.to_account_info(),
        },
        &[&[APP_AUTHORITY_SEED, &[bump]]],
    ),
    loser,
    winner,
    amount,
)?;
```

Instructions that take a variable list of accounts (`cash_out`, `refund_pot`, `settle`, `set_guardians`, `unfreeze_app`) read them from the remaining accounts. Over CPI, pass them with `CpiContext::with_remaining_accounts`.

### Where instructions run

Most instructions run on Solana. `move_seat` and `close_pot` run in the MagicBlock rollup, where the pot is delegated. The program is built with `#[ephemeral]`, which also adds the SDK's `process_undelegation` instruction (see [3.6](#36-sdk-generated-process_undelegation)).

---

## 2. Accounts

All amounts are in zatoshi (1 ZEC = 100,000,000 zatoshi). "App PDA" below means the `App` account address, not the app's program ID.

### PDA seeds

| Account | Seeds | Owner program | Delegated? |
|---|---|---|---|
| `Config` | `["config"]` | ledger | No |
| `App` | `["app", app_program_id]` | ledger | No |
| `Deposit` | `["deposit", deposit_id]` | ledger | No |
| `Balance` | `["balance", app_pda, session_key]` | ledger | No |
| `Pot` | `["pot", app_pda, pot_id]` | ledger | Yes: from `delegate_pot` until `close_pot` |
| `PotReceipt` | `["receipt", pot_pda]` | ledger | No; never leaves Solana |
| `Settlement` | `["settlement", app_pda, sequence.to_le_bytes()]` (`sequence` is a `u64`) | ledger | No |
| App authority | `["zol-app"]` | the app program | n/a (not a ledger account) |

Notes:

- `Deposit` seeds hold only the deposit ID, not the app. Deposit IDs are global across apps.
- `Pot` is the only account that is ever delegated. While delegated it is owned by the delegation program `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`.
- No ledger instruction closes an account. Rent stays in `Deposit`, `Balance`, `Pot`, `PotReceipt` and `Settlement` accounts.

### `Config`

One per deployment. Created by `initialize`.

| Field | Type | Meaning |
|---|---|---|
| `admin` | `Pubkey` | Can call `set_config`, `set_cap`, `activate_app`. |
| `attestor` | `Pubkey` | FROST Ed25519 group key of the attestors. Signs credits, voids and paid records. |
| `guardians` | `[Pubkey; 7]` | Guardian set; the first `guardian_count` entries are used. |
| `guardian_count` | `u8` | Number of guardians. |
| `guardian_threshold` | `u8` | Guardian signatures needed for `set_guardians` and `unfreeze_app`. |
| `network` | `u8` | Zcash network, as `zol_wire::Network`: `0` Mainnet, `1` Testnet, `2` Regtest. Bound into every attested digest. |
| `paused` | `bool` | When true, every instruction that requires an active app fails with `Paused`. |
| `params` | `Params` | See below. |
| `bump` | `u8` | PDA bump. |

### `Params`

Set at `initialize` and replaceable by `set_config`. `Params::validate` enforces the rules in the last column.

| Field | Type | Meaning | Validation |
|---|---|---|---|
| `entry_fee_bps` | `u16` | Fee taken from each credited deposit, in basis points. | `<= MAX_FEE_BPS` (100) |
| `exit_fee_bps` | `u16` | Fee taken from each payout at `settle`, in basis points. | `<= MAX_FEE_BPS` (100) |
| `network_fee_allowance` | `u64` | Fixed amount deducted from each payout at `settle` for the Zcash network fee. | `<= MAX_NETWORK_FEE_ALLOWANCE` (100,000) |
| `min_exit` | `u64` | Smallest balance that can request an exit or be settled. | `min_exit - floor(min_exit * exit_fee_bps / 10000) - network_fee_allowance > 0` |
| `max_seats` | `u8` | Most seats a pot can have. | `2 <= max_seats <= MAX_SEATS` (16) |
| `max_open_secs` | `i64` | Latest `open_deadline` allowed, relative to `open_pot` time. | `> 0` |
| `min_session_secs` | `i64` | Earliest `session_deadline` allowed, relative to `delegate_pot` time. | `> 0` and `<= max_session_secs` |
| `max_session_secs` | `i64` | Latest `session_deadline` allowed, relative to `delegate_pot` time. | see above |
| `refund_grace_secs` | `i64` | Wait after `session_deadline` before a delegated pot that has not validly returned can be refunded. Must exceed MagicBlock's longest commit-to-return time. | `>= MIN_REFUND_GRACE_SECS` |
| `activation_delay_secs` | `i64` | Delay between `activate_app` and the app becoming `Active`, so guardians can cancel. | `>= MIN_ACTIVATION_DELAY_SECS` |
| `registration_bond` | `u64` | Lamports moved from the payer into the `App` account at registration. | none |
| `denominations` | `[u64; 4]` | Permitted deposit values in zatoshi; `0` marks an unused slot. Apps may read it (coin-flip derives its stakes from it). | At least one non-zero; no duplicates; each non-zero value `>= min_exit` |

`bps(amount, bps)` is `floor(amount * bps / 10_000)`, computed in `u128`.

### `limits` constants

From `zol_ledger::state::limits`. The admin can only set parameters within these bounds.

| Constant | Value | Meaning |
|---|---|---|
| `MAX_GUARDIANS` | 7 | Size of the guardian set. |
| `MAX_SEATS` | 16 | Seats per pot (array size). |
| `MAX_ACTION_ACCOUNTS` | 16 | Accounts in a `close_pot` return action. |
| `MAX_ACTION_DATA` | 256 | Bytes of instruction data in a return action. |
| `MAX_ACTION_COMPUTE_UNITS` | 400,000 | Compute units of a return action. |
| `MAX_PAYOUTS` | 16 | Payouts per settlement (`zol_wire::MAX_PAYOUTS_PER_SETTLEMENT`). |
| `MAX_FEE_BPS` | 100 | 1% cap on each of the entry and exit fee. |
| `MAX_NETWORK_FEE_ALLOWANCE` | 100,000 | 0.001 ZEC cap on the per-payout network fee allowance. |
| `DEV_TIMINGS` | `cfg!(feature = "dev-timings")` | True in a local development build. |
| `MIN_ACTIVATION_DELAY_SECS` | 3,600 (5 with `dev-timings`) | Floor on `activation_delay_secs`. |
| `MIN_REFUND_GRACE_SECS` | 3,900 (10 with `dev-timings`) | Floor on `refund_grace_secs`. A forced undelegation completes only after 9,000 slots (about 60 minutes). |
| `MAX_APP_DATA` | 329 | Bytes of `app_data` in a credit (`zol_wire::memo::APP_DATA_MAX`). |
| `MAX_DENOMINATIONS` | 4 | Slots in `Params.denominations`. |

A `dev-timings` build refuses to `initialize` for Zcash mainnet.

### `App`

One per app program. Created by `register_app`.

| Field | Type | Meaning |
|---|---|---|
| `program_id` | `Pubkey` | The app's program ID. |
| `authority` | `Pubkey` | The app's `["zol-app"]` PDA; signs on the app's behalf. |
| `registrant` | `Pubkey` | Payer of `register_app`. |
| `state` | `AppState` | `Pending`, `Activating`, `Active` or `Frozen`. |
| `activates_at` | `i64` | Unix time at which `Activating` becomes `Active`. |
| `vault_address` | `[u8; 43]` | Raw Orchard-format receiver of the app's Zcash vault. |
| `genesis_head` | `[u8; 32]` | Note commitment (cmx) of the vault's genesis head. |
| `deposit_cap` | `u64` | Credits are refused once `liability + unpaid_total` would exceed it. Starts at 0. |
| `liability` | `u64` | `sum(Balance.amount) + sum(live PotReceipt.total)`. |
| `deposit_count` | `u64` | Number of credited deposits; the next `Deposit.index`. |
| `credited_total` | `u64` | Sum of `Deposit.amount` over credited deposits (gross, before the entry fee). |
| `fee_pool` | `u64` | Entry fees, exit fees and network fee allowances collected. No ledger instruction draws on it. |
| `next_settlement` | `u64` | Sequence of the next `Settlement`. |
| `paid_count` | `u64` | Number of settlements marked paid; also the next sequence `mark_paid` accepts. |
| `unpaid_total` | `u64` | Sum of `total` over unpaid settlements. |
| `bond` | `u64` | Registration bond paid, in lamports. No ledger instruction returns it. |
| `bump` | `u8` | PDA bump. |

`AppState`:

| Variant | Meaning |
|---|---|
| `Pending` | Registered, no vault yet. Also the state after a guardian cancels an activation. |
| `Activating` | Vault recorded; becomes `Active` at `activates_at` unless a guardian cancels. |
| `Active` | Normal operation. |
| `Frozen` | Frozen by a guardian. Only a guardian threshold returns it to `Active`. |

The move from `Activating` to `Active` is lazy. `App::refresh` applies it at the start of `credit_deposit`, `open_pot`, `buy_in`, `delegate_pot`, `settle` and `freeze_app`. A stored `Activating` with `activates_at <= now` is effectively `Active`. Off-chain readers should treat it so.

### `Deposit`

One per deposit ID. Created by either `credit_deposit` or `void_deposit`, so each ID is credited or voided at most once.

| Field | Type | Meaning |
|---|---|---|
| `id` | `[u8; 32]` | The deposit ID (also the seed). |
| `state` | `DepositState` | `Credited` or `Voided`. |
| `app` | `Pubkey` | App PDA. |
| `index` | `u64` | Position among this app's credited deposits; meaningless when voided. |
| `amount` | `u64` | Full note value (gross). Unset when voided. |
| `session_key` | `Pubkey` | Session key credited. Unset when voided. |
| `payout_commitment` | `[u8; 32]` | Commitment to the player's payout address. Unset when voided. |
| `pot_id` | `[u8; 32]` | Pot ID from the deposit memo. Stored only; the ledger does not act on it. Unset when voided. |
| `digest` | `[u8; 32]` | The digest the attestor signed (credit or void). |
| `bump` | `u8` | PDA bump. |

### `Balance`

One per (app, session key). Created by the first `credit_deposit` for that session key.

| Field | Type | Meaning |
|---|---|---|
| `app` | `Pubkey` | App PDA. |
| `session_key` | `Pubkey` | Player's session key. Signs `buy_in`, `leave_pot`, `request_exit`. |
| `amount` | `u64` | Spendable balance. |
| `payout_commitment` | `[u8; 32]` | Set by the first credit. Later credits must carry the same value. Copied into settlement payouts. |
| `exiting` | `bool` | Marked for exit. Blocks `buy_in`. Cleared by `settle`. |
| `auto_exit` | `bool` | From the deposit flags (`FLAG_AUTO_EXIT = 0x01`), overwritten by each credit. When set, `cash_out` marks the balance exiting once it reaches `min_exit`. |
| `bump` | `u8` | PDA bump. |

### `Pot`

The part of a pot that goes to the rollup. Holds amounts per seat, never player keys. Created by `open_pot`.

| Field | Type | Meaning |
|---|---|---|
| `app` | `Pubkey` | App PDA. |
| `app_authority` | `Pubkey` | Copy of `App.authority`, so rollup instructions need no undelegated account. |
| `pot_id` | `[u8; 32]` | App-chosen ID (also a seed). |
| `seat_count` | `u8` | Seats in use. |
| `amounts` | `[u64; 16]` | Amount per seat, in receipt order. |
| `delegated` | `bool` | Set by `delegate_pot`, cleared by `close_pot`. Seats may move only while set. |
| `bump` | `u8` | PDA bump. |

Delegable: yes. `delegate_pot` delegates it to the named validator; `close_pot` commits and undelegates it.

### `PotReceipt`

The Solana-side record of a pot: who sits where and what they paid. Never delegated. Created by `open_pot`.

| Field | Type | Meaning |
|---|---|---|
| `app` | `Pubkey` | App PDA. |
| `pot` | `Pubkey` | Pot PDA. |
| `pot_id` | `[u8; 32]` | Pot ID. |
| `state` | `PotState` | `Open`, `Delegated`, `CashedOut` or `Refunded`. |
| `seat_count` | `u8` | Seats in use. |
| `players` | `[Pubkey; 16]` | Session key per seat. |
| `buy_ins` | `[u64; 16]` | Total bought in per seat. |
| `total` | `u64` | Sum of `buy_ins`. |
| `open_deadline` | `i64` | Buy-ins and delegation must happen before it. After it, an `Open` pot can be refunded. |
| `session_deadline` | `i64` | Set at delegation. A pot not validly returned by `session_deadline + refund_grace_secs` can be refunded. |
| `validator` | `Pubkey` | Rollup validator named at delegation. The delegation program accepts commits only from it, so it is trusted for this pot's outcome. |
| `bump` | `u8` | PDA bump. |

`PotState` transitions:

```
open_pot ──> Open ──delegate_pot──> Delegated ──cash_out──> CashedOut
              │                        │
              └──refund_pot──> Refunded <──refund_pot──┘
```

A receipt is "live" in `Open` and `Delegated`.

### `Settlement`

One per settle call, numbered per app from 0. Created by `settle`.

| Field | Type | Meaning |
|---|---|---|
| `app` | `Pubkey` | App PDA. |
| `sequence` | `u64` | Position in the app's settlements (also a seed). |
| `state` | `SettlementState` | `Unpaid` or `Paid`. |
| `count` | `u8` | Payouts in use (1 to 16). |
| `payouts` | `[PayoutEntry; 16]` | First `count` entries are the payouts; the rest are zero. |
| `total` | `u64` | Sum of payout amounts. |
| `bump` | `u8` | PDA bump. |

### Plain structs

`PayoutEntry`:

| Field | Type | Meaning |
|---|---|---|
| `commitment` | `[u8; 32]` | The balance's `payout_commitment`. |
| `amount` | `u64` | Net payout: balance minus exit fee minus network fee allowance. |

`SeatStake` (argument of `delegate_pot`):

| Field | Type | Meaning |
|---|---|---|
| `player` | `Pubkey` | Expected session key at this seat. |
| `amount` | `u64` | Expected buy-in at this seat. |

`PotAction` (argument of `close_pot`): an instruction the delegation program runs on Solana right after the pot returns, signed only by the app's escrow (its fee payer). It may call only the pot's own app program. The ledger never signs for it.

| Field | Type | Meaning |
|---|---|---|
| `program` | `Pubkey` | Program to call. Must be the pot's app program. |
| `accounts` | `Vec<ActionAccount>` | Accounts of the instruction, in order. At most 16. |
| `data` | `Vec<u8>` | Instruction data. At most 256 bytes. |
| `compute_units` | `u32` | Compute budget for the action. 1 to 400,000. |

`ActionAccount`:

| Field | Type | Meaning |
|---|---|---|
| `pubkey` | `Pubkey` | Account address. |
| `is_writable` | `bool` | Writable flag. Action accounts carry no signer flag. |

Instruction argument structs (`InitializeArgs`, `SetConfigArgs`, `CreditArgs`) are described with their instructions.

---

## 3. Instructions

Account tables list accounts in the order of the IDL. **W** = writable, **S** = signer. Anchor's own account checks (wrong PDA, wrong owner, failed deserialization, account already initialized) fail with Anchor or System Program errors, not with the ledger errors listed per instruction.

### Which instructions an app's state blocks

`App::require_active` fails with `Paused` if `Config.paused`, `AppFrozen` if the app is `Frozen`, and `AppNotActive` otherwise unless `Active`. It is called by:

| Requires an active app and unpaused ledger | Works in any app state and while paused |
|---|---|
| `credit_deposit`, `open_pot`, `buy_in`, `delegate_pot`, `settle` | `void_deposit`, `leave_pot`, `move_seat`, `close_pot`, `cash_out`, `refund_pot`, `request_exit`, `mark_paid` |

So players can always leave an open pot, pots in the rollup can always come home and be paid out or refunded, and exits can be requested. Settling needs the app active again.

### Attestations

`credit_deposit`, `void_deposit` and `mark_paid` require that the **instruction immediately before** them in the transaction is an Ed25519 precompile (`Ed25519SigVerify111111111111111111111111111`) instruction that:

- verifies exactly one signature (`num_signatures == 1`);
- takes its signature, public key and message from its own data (all three instruction indexes are `u16::MAX`);
- has public key `Config.attestor`;
- has a 32-byte message equal to the expected digest.

Otherwise the instruction fails with `BadAttestation`. The digests are defined in [`crates/zol-wire/src/digest.rs`](../../crates/zol-wire/src/digest.rs). Each is a SHA-256 over a 16-byte domain tag and the network byte, then:

| Instruction | Digest | Hashed after tag and network |
|---|---|---|
| `credit_deposit` | `CreditAttestation::digest` (tag `ZOL/credit/v1`) | app program ID, `deposit_id`, `amount` (u64 LE), `session_key`, `payout_commitment`, `flags`, `pot_id`, `app_data` length (u16 LE), `app_data` |
| `void_deposit` | `void_digest` (tag `ZOL/void/v1`) | app program ID, `deposit_id` |
| `mark_paid` | `paid_digest` (tag `ZOL/paid/v1`) | app program ID, `sequence` (u64 LE) |

### 3.1 Admin and guardians

#### `initialize`

Runs on Solana. Creates `Config`. Only the program's upgrade authority can call it, once.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `payer` | W | S | Must be the program's upgrade authority. Pays rent. |
| 1 | `config` | W | | `Config` PDA, created. |
| 2 | `program` | | | This program; its `programdata_address` must be `program_data`. |
| 3 | `program_data` | | | The program's `ProgramData` account. |
| 4 | `system_program` | | | System Program. |

Args: `args: InitializeArgs`

| Field | Type | Meaning |
|---|---|---|
| `admin` | `Pubkey` | Initial admin. |
| `attestor` | `Pubkey` | Attestor group key. |
| `guardians` | `Vec<Pubkey>` | Guardian set. |
| `guardian_threshold` | `u8` | Guardian threshold. |
| `network` | `u8` | Zcash network (`0`, `1` or `2`). |
| `params` | `Params` | Initial parameters. |

Checks: `params` pass `Params::validate`. Guardians are 1 to 7, distinct, none the default key, and `1 <= threshold <= len`. `network` is a known value. A `dev-timings` build rejects mainnet.

Effect: writes all fields; `paused = false`.

Errors: `NotUpgradeAuthority`, `FeeTooHigh`, `BadParams`, `BadGuardians`.

#### `set_config`

Runs on Solana. Signer: admin. The admin can change parameters within the program's caps, rotate the attestor key, pause, and hand over the admin role. It cannot change guardians and cannot unfreeze an app.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `admin` | | S | Must equal `Config.admin`. |
| 1 | `config` | W | | `Config` PDA. |

Args: `args: SetConfigArgs`. Each `None` field is left unchanged.

| Field | Type | Meaning |
|---|---|---|
| `new_admin` | `Option<Pubkey>` | New admin. |
| `attestor` | `Option<Pubkey>` | New attestor key. |
| `params` | `Option<Params>` | New parameters; validated with `Params::validate`. |
| `paused` | `Option<bool>` | Pause or unpause. |

Emits `ConfigChanged`. Errors: `FeeTooHigh`, `BadParams`; `has_one = admin` failure is an Anchor error.

#### `set_guardians`

Runs on Solana. Replaces the guardian set. Signers: at least `guardian_threshold` distinct current guardians.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `config` | W | | `Config` PDA. |
| rest | guardians | | S | Current guardians, as remaining accounts. Non-guardians, non-signers and duplicates are not counted. |

Args: `guardians: Vec<Pubkey>`, `threshold: u8`. Same rules as at `initialize`.

Errors: `GuardianThreshold`, `BadGuardians`. No event.

#### `set_cap`

Runs on Solana. Signer: admin. Sets `App.deposit_cap` to any value. Works in any app state.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `admin` | | S | Must equal `Config.admin`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |

Args: `cap: u64`. No checks beyond the admin. No event.

A new app's cap is 0, so it accepts no credits until the admin sets a cap.

#### `freeze_app`

Runs on Solana. Signer: any one guardian.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `guardian` | | S | Must be in `Config.guardians`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |

Args: none.

Effect, after `App::refresh`:

- `Activating` (still within its delay): cancels the activation. State back to `Pending`; `vault_address`, `genesis_head` and `activates_at` zeroed. Emits `ActivationCancelled`.
- `Active`: state becomes `Frozen`. Emits `AppFrozen`.
- Any other state: fails.

Errors: `NotGuardian`, `BadAppState`.

#### `unfreeze_app`

Runs on Solana. Returns a frozen app to `Active`. Signers: at least `guardian_threshold` distinct guardians. The admin cannot do this.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `config` | | | `Config` PDA. |
| 1 | `app` | W | | `App` PDA. |
| rest | guardians | | S | Guardians, as remaining accounts. |

Args: none. Emits `AppUnfrozen`. Errors: `GuardianThreshold`, `BadAppState` (app not `Frozen`).

### 3.2 App registration and activation

#### `register_app`

Runs on Solana. Creates the `App` account in state `Pending`. The app must call it by CPI, signing for its `["zol-app"]` PDA; this is the app's consent. Anyone may pay.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `payer` | W | S | Pays rent and the registration bond. Recorded as `registrant`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app_program` | | | The app program. Must be executable. |
| 3 | `app_authority` | | S | Must be `find_program_address(["zol-app"], app_program)`. |
| 4 | `app` | W | | `App` PDA `["app", app_program]`, created. |
| 5 | `system_program` | | | System Program. |

Args: none.

Effect: if `registration_bond > 0`, transfers that many lamports from `payer` to the `App` account. Sets `program_id`, `authority`, `registrant`, `state = Pending`, `bond`. All counters and `deposit_cap` start at 0. Emits `AppRegistered`.

Errors: `NotAProgram`, `BadAppAuthority`.

#### `activate_app`

Runs on Solana. Signer: admin. Records the vault and starts the activation delay. The vault address and genesis head never change once the app is active. Only a guardian cancel during the delay clears them.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `admin` | | S | Must equal `Config.admin`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |

Args: `vault_address: [u8; 43]`, `genesis_head: [u8; 32]`.

Checks: app is `Pending`; neither argument is all zeros.

Effect: state `Activating`; `activates_at = now + activation_delay_secs`. Emits `AppActivating`.

Errors: `BadAppState`, `BadParams`, `Overflow`.

### 3.3 Deposits

#### `credit_deposit`

Runs on Solana. Credits an attested deposit to the player's balance. Any account may relay it; the attestation is the authorization.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `relayer` | W | S | Pays rent for `deposit` and a new `balance`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |
| 3 | `deposit` | W | | `Deposit` PDA `["deposit", args.deposit_id]`, created. |
| 4 | `balance` | W | | `Balance` PDA `["balance", app, args.session_key]`, created if needed. |
| 5 | `instructions` | | | Instructions sysvar. |
| 6 | `system_program` | | | System Program. |

Args: `args: CreditArgs`

| Field | Type | Meaning |
|---|---|---|
| `deposit_id` | `[u8; 32]` | Deposit ID. |
| `amount` | `u64` | Full note value in zatoshi. |
| `session_key` | `Pubkey` | Player's session key. |
| `payout_commitment` | `[u8; 32]` | Commitment to the player's payout address. |
| `flags` | `u8` | Deposit flags. Only `FLAG_AUTO_EXIT` (`0x01`) is known. |
| `pot_id` | `[u8; 32]` | Pot ID from the memo. Stored in `Deposit`; not acted on. |
| `app_data` | `Vec<u8>` | App data from the memo, at most 329 bytes. Part of the digest; not stored. |

Checks, in order: app active and ledger unpaused; `amount > 0`; `amount` is a permitted denomination; no unknown flag bits; `app_data` within the limit; the preceding instruction attests the credit digest; `liability + unpaid_total + net <= deposit_cap`; for an existing balance, the same `payout_commitment`.

Effect:

- `fee = bps(amount, entry_fee_bps)`, `net = amount - fee`.
- `App`: `deposit_count += 1`, `credited_total += amount`, `liability += net`, `fee_pool += fee`.
- `Deposit`: state `Credited`, `index` = previous `deposit_count`, and the arguments.
- `Balance`: created on first use with this `payout_commitment`; `amount += net`; `auto_exit` set from `flags`.
- Emits `DepositCredited`.

A second credit or void of the same `deposit_id` fails because the `Deposit` account already exists.

Errors: `Paused`, `AppFrozen`, `AppNotActive`, `ZeroAmount`, `BadDenomination`, `BadFlags`, `AppDataTooLong`, `BadAttestation`, `OverCap`, `PayoutCommitmentMismatch`, `Overflow`.

#### `void_deposit`

Runs on Solana. Records an attested void, so the deposit can never be credited, and the operators can refund it on Zcash. Works in any app state, including frozen, paused or cancelled. Credits nothing.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `relayer` | W | S | Pays rent for `deposit`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | | | `App` PDA. |
| 3 | `deposit` | W | | `Deposit` PDA `["deposit", deposit_id]`, created. |
| 4 | `instructions` | | | Instructions sysvar. |
| 5 | `system_program` | | | System Program. |

Args: `deposit_id: [u8; 32]`.

Effect: creates the `Deposit` with state `Voided`, `app`, `id` and `digest`. Emits `DepositVoided`.

Errors: `BadAttestation`.

### 3.4 Pots

A pot's life:

1. `open_pot` (app) creates `Pot` and `PotReceipt` in state `Open`.
2. `buy_in` (app and player) moves balance into a seat. `leave_pot` (player alone) moves it back.
3. `delegate_pot` (app) checks the seats, fixes `session_deadline` and the validator, and delegates `Pot` to the rollup.
4. `move_seat` (app, in the rollup) moves value between seats.
5. `close_pot` (app, in the rollup) commits the pot and returns it to Solana, optionally with a return action.
6. `cash_out` (anyone) pays the returned seat amounts to the players' balances.

`refund_pot` (anyone) returns every buy-in if the pot never started in time, or never validly returned.

#### `open_pot`

Runs on Solana. Signer: app authority.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `payer` | W | S | Pays rent for `pot` and `receipt`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |
| 3 | `authority` | | S | Must equal `App.authority`. |
| 4 | `pot` | W | | `Pot` PDA `["pot", app, pot_id]`, created. |
| 5 | `receipt` | W | | `PotReceipt` PDA `["receipt", pot]`, created. |
| 6 | `system_program` | | | System Program. |

Args: `pot_id: [u8; 32]`, `open_deadline: i64`.

Checks: app active and ledger unpaused; `now < open_deadline <= now + max_open_secs`. `pot_id` must be unused for this app.

Effect: `Pot` with `app`, `app_authority`, `pot_id`, no seats. `PotReceipt` in state `Open` with `open_deadline`. Emits `PotOpened`.

Errors: `BadAppAuthority`, `Paused`, `AppFrozen`, `AppNotActive`, `BadDeadline`, `Overflow`.

#### `buy_in`

Runs on Solana. Signers: app authority and the player's session key. Nobody enters a pot except through the app.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `config` | | | `Config` PDA. |
| 1 | `app` | W | | `App` PDA. |
| 2 | `authority` | | S | Must equal `App.authority`. |
| 3 | `session_key` | | S | The player. |
| 4 | `balance` | W | | `Balance` PDA `["balance", app, session_key]`. |
| 5 | `pot` | W | | `Pot` PDA `["pot", app, pot.pot_id]`. |
| 6 | `receipt` | W | | `PotReceipt` PDA `["receipt", pot]`. |

Args: `amount: u64`.

Checks: app active and ledger unpaused; receipt `Open`; `now < open_deadline`; `amount > 0`; balance not `exiting`; `balance.amount >= amount`. A new player needs a free seat: `seat_count < max_seats`.

Effect: `balance.amount -= amount`. A player already seated adds to their seat; a new player takes the next seat. `receipt.buy_ins[seat]` and `receipt.total` grow by `amount`. `pot.amounts[seat]` is set to the seat's buy-in and `pot.seat_count` to the receipt's. Emits `BoughtIn`.

Errors: `BadAppAuthority`, `Paused`, `AppFrozen`, `AppNotActive`, `BadPotState`, `OpenDeadlinePassed`, `ZeroAmount`, `BalanceExiting`, `InsufficientBalance`, `PotFull`, `Overflow`.

#### `leave_pot`

Runs on Solana. Signer: the player's session key alone. While the pot is `Open`, a player can always take their whole buy-in back, so an app cannot trap anyone in a pot that has not started. Works in any app state and while paused.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `session_key` | | S | The player. |
| 1 | `balance` | W | | `Balance` PDA `["balance", receipt.app, session_key]`. |
| 2 | `pot` | W | | `Pot` PDA `["pot", receipt.app, receipt.pot_id]`. |
| 3 | `receipt` | W | | `PotReceipt` PDA `["receipt", pot]`. |

Args: none.

Checks: receipt `Open`; the player has a seat.

Effect: removes the seat and shifts later seats down by one in both receipt and pot, so they stay aligned and ordered. `receipt.total` and `seat_count` decrease. The buy-in returns to `balance.amount`. Emits `LeftPot`.

Because seats shift, an app must not assume a player's seat index is fixed before delegation. `delegate_pot` takes the expected seating for this reason.

Errors: `BadPotState`, `NoSeat`, `Overflow`.

#### `delegate_pot`

Runs on Solana. Signer: app authority. Starts the pot: fixes the seating, the session deadline and the rollup validator, then delegates `Pot` to the MagicBlock delegation program.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `payer` | W | S | Pays for the delegation accounts. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |
| 3 | `authority` | | S | Must equal `App.authority`. |
| 4 | `receipt` | W | | `PotReceipt` PDA `["receipt", pot]`; `receipt.app` must be `app`. |
| 5 | `buffer_pot` | W | | Delegation buffer, PDA `["buffer", pot]` under the ledger program. |
| 6 | `delegation_record_pot` | W | | PDA `["delegation", pot]` under the delegation program. |
| 7 | `delegation_metadata_pot` | W | | PDA `["delegation-metadata", pot]` under the delegation program. |
| 8 | `pot` | W | | The pot; must equal `receipt.pot` and be owned by the ledger. |
| 9 | `owner_program` | | | The ledger program, `3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y`. |
| 10 | `delegation_program` | | | `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`. |
| 11 | `system_program` | | | System Program. |

Accounts 5–7 and 9–11 are added by the ephemeral-rollups SDK's `#[delegate]` macro.

Args:

| Arg | Type | Meaning |
|---|---|---|
| `expected` | `Vec<SeatStake>` | The seating the app expects, in seat order. Players can leave until the start, so the app must state who it expects. |
| `session_deadline` | `i64` | When the session should end. |
| `validator` | `Pubkey` | Rollup validator to delegate to. Must not be the default key: without an explicit validator the delegation program picks its default one, and the pot never appears in the app's rollup. |

Checks: `validator` non-default; app active and ledger unpaused; receipt `Open`; `now < open_deadline`; `seat_count > 0`; `expected` matches `players` and `buy_ins` seat by seat and has the same length; `now + min_session_secs <= session_deadline <= now + max_session_secs`; pot owned by the ledger with the receipt's `seat_count`.

Effect: receipt state `Delegated`, `session_deadline` and `validator` recorded; `pot.delegated = true`; pot delegated to `validator`. Emits `PotDelegated`.

Errors: `BadParams`, `BadAppAuthority`, `BadAccount`, `Paused`, `AppFrozen`, `AppNotActive`, `BadPotState`, `OpenDeadlinePassed`, `ReceiptMismatch`, `BadDeadline`, `PotInvalid`, `Overflow`.

#### `move_seat`

Runs **in the rollup**. Signer: app authority. Moves value between two seats of one delegated pot. It cannot create value, add a player or touch another pot. It reads no `Config` or `App`, so app freezes and the ledger pause do not stop it.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `authority` | | S | The app's `["zol-app"]` PDA. |
| 1 | `pot` | W | | Delegated `Pot` PDA `["pot", pot.app, pot.pot_id]`. |
| 2 | `app_authority` | | | Must equal `pot.app_authority` and `authority`. Pass the app authority again. |

Args: `from: u8`, `to: u8`, `amount: u64`.

Checks: `pot.delegated`; `from < seat_count`, `to < seat_count`, `from != to`; `amounts[from] >= amount`. A zero `amount` is not rejected.

Effect: `amounts[from] -= amount`, `amounts[to] += amount`. No event.

Errors: `BadAppAuthority`, `BadPotState`, `BadSeat`, `InsufficientBalance`, `Overflow`.

#### `close_pot`

Runs **in the rollup**. Signer: app authority (and the payer). Commits the pot and returns it to Solana. With `action`, the delegation program then runs that instruction on Solana right after the pot is back, paid from the app's escrow. An app can use this to pay its players out without a keeper. `close_pot` checks no deadline; the app decides when to close.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `payer` | W | S | Pays the commit. May be a delegated account signing by seeds (for example an app PDA in a scheduled task); then pass `magic_fee_vault`. |
| 1 | `authority` | | S | Must equal `pot.app_authority`. |
| 2 | `pot` | W | | The delegated `Pot`. Must be owned by the ledger (in the rollup) and be the PDA `["pot", pot.app, pot.pot_id]`. |
| 3 | `magic_fee_vault` | W | | Optional. MagicBlock's fee vault for the validator; the magic program checks it. Needed when the payer is itself a delegated account. zol-coinflip derives it as `["magic-fee-vault", validator]` under `ephemeral_rollups_sdk::id()`. |
| 4 | `escrow_authority` | | S | Optional. The app's escrow authority for `action` (the app signs for its PDA). Required when `action` is set. |
| 5 | `magic_program` | | | `Magic11111111111111111111111111111111111111`. |
| 6 | `magic_context` | W | | `MagicContext1111111111111111111111111111111`. |

Accounts 5–6 are added by the SDK's `#[commit]` macro. In Anchor, omit an optional account by passing the program ID in its place; `zol_ledger::cpi::accounts::ClosePot` takes `Option<AccountInfo>` for both.

Args: `action: Option<PotAction>`.

Checks: pot owned by the ledger and at the right address; `authority == pot.app_authority`; `pot.delegated`. With an action:

- `find_program_address(["zol-app"], action.program)` equals `authority`, so the action can only call the pot's own app;
- `action.accounts.len() <= 16`;
- `action.data.len() <= 256`;
- `0 < action.compute_units <= 400_000`;
- `escrow_authority` is present and a signer.

The ledger does not check the address of `escrow_authority`. Action accounts carry no signer flag; the action runs signed only by the escrow.

Effect: `pot.delegated = false` is written, then a MagicBlock intent bundle commits and undelegates the pot, with `magic_fee_vault` if given. With an action, the bundle adds it as a post-undelegate action (`CallHandler`) with `destination_program = action.program`, the action's accounts, data and compute units, and `escrow_authority`. The return to Solana is asynchronous. Until it lands, `cash_out` fails with `PotNotReturned`. An app's action handler should expect the pot may not be back yet; zol-coinflip's `cash_out_table` then does nothing. zol-coinflip also notes that the action receives extra trailing accounts (the escrow), which its handler ignores. No event.

Errors: `BadAccount`, `BadAppAuthority`, `BadPotState`, `BadAction`.

#### `cash_out`

Runs on Solana. Anyone may call it, including an app's return action. Pays the returned seat amounts to the players' balances. Works in any app state and while paused, so pots in the rollup can still come home.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `config` | | | `Config` PDA. |
| 1 | `app` | | | `App` PDA. |
| 2 | `receipt` | W | | `PotReceipt` PDA `["receipt", pot]`; `receipt.app` must be `app`. |
| 3 | `pot` | | | `Pot` PDA `["pot", app, receipt.pot_id]`. Owner and contents checked by hand. |
| rest | balances | W | | One `Balance` per seat, in receipt seat order, as remaining accounts. |

Args: none.

Checks: receipt `Delegated`; pot owned by the ledger again (otherwise `PotNotReturned`). The pot must have validly returned: `seat_count`, `app` and `pot_id` match the receipt, the seat amounts sum to `receipt.total`, and unused seats are zero (otherwise `PotInvalid`). Exactly `seat_count` balances, no duplicates, each with `app == app` and `session_key == players[i]`.

Effect: `balances[i].amount += pot.amounts[i]`. If a balance has `auto_exit` and now holds at least `min_exit`, it is marked `exiting`. Receipt state `CashedOut`. Emits `PotCashedOut`. `App.liability` does not change (value moves from a live receipt to balances).

Errors: `BadAccount`, `BadPotState`, `PotNotReturned`, `PotInvalid`, `DuplicateAccount`, `Overflow`.

#### `refund_pot`

Runs on Solana. Anyone. Returns every buy-in from the receipt. Works in any app state and while paused. Same accounts as `cash_out`.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `config` | | | `Config` PDA. |
| 1 | `app` | | | `App` PDA. |
| 2 | `receipt` | W | | `PotReceipt` PDA `["receipt", pot]`; `receipt.app` must be `app`. |
| 3 | `pot` | | | `Pot` PDA `["pot", app, receipt.pot_id]`. |
| rest | balances | W | | One `Balance` per seat, in receipt seat order. |

Args: none.

Allowed when:

- receipt `Open` and `now >= open_deadline` (the pot never started in time); or
- receipt `Delegated`, `now >= session_deadline + refund_grace_secs`, and the pot has not validly returned (same test as `cash_out`). A pot that returned and passes the checks can only be cashed out.

Effect: `balances[i].amount += receipt.buy_ins[i]`; `auto_exit` is not applied. Receipt state `Refunded`. Emits `PotRefunded`.

Errors: `BadAccount`, `RefundNotAllowed`, `DuplicateAccount`, `Overflow`.

### 3.5 Exits and settlements

#### `request_exit`

Runs on Solana. Signer: the player's session key. Marks the balance for payout. The whole balance at `settle` time is paid, and buy-ins are refused while it is marked. Works in any app state and while paused. There is no instruction to unmark.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `session_key` | | S | The player. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `balance` | W | | `Balance` PDA `["balance", balance.app, session_key]`. |

Args: none. Checks: `balance.amount >= min_exit`. Effect: `exiting = true`. Emits `ExitRequested`.

Errors: `BelowMinExit`.

#### `settle`

Runs on Solana. Anyone. Turns 1 to 16 exiting balances into the app's next settlement record, which the operators pay on Zcash.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `payer` | W | S | Pays rent for `settlement`. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |
| 3 | `settlement` | W | | `Settlement` PDA `["settlement", app, app.next_settlement]`, created. |
| 4 | `system_program` | | | System Program. |
| rest | balances | W | | 1 to 16 `Balance` accounts of this app, as remaining accounts. |

Args: none.

Checks: app active and ledger unpaused; 1 to 16 balances; no duplicates; each belongs to `app`, is `exiting`, and holds at least `min_exit`.

Effect, per balance: `fee = bps(amount, exit_fee_bps) + network_fee_allowance`; `payouts[i] = { commitment: payout_commitment, amount: amount - fee }`; `App.liability -= amount`; `App.fee_pool += fee`; the balance's `amount = 0` and `exiting = false`. Then `Settlement` is written as `Unpaid` with `sequence = next_settlement`, `count` and `total`; `next_settlement += 1`; `unpaid_total += total`. Emits `Settled`.

Errors: `Paused`, `AppFrozen`, `AppNotActive`, `BadPayoutCount`, `DuplicateAccount`, `BadAccount`, `NotExiting`, `BelowMinExit`, `Overflow`.

#### `mark_paid`

Runs on Solana. Any relayer, with an attestation. A public record for auditors, posted after the payout reaches depth on Zcash. The code notes that signers never rely on it. Works in any app state and while paused.

| # | Account | W | S | Description |
|---|---|---|---|---|
| 0 | `relayer` | | S | Any signer. |
| 1 | `config` | | | `Config` PDA. |
| 2 | `app` | W | | `App` PDA. |
| 3 | `settlement` | W | | `Settlement` PDA `["settlement", app, settlement.sequence]`. |
| 4 | `instructions` | | | Instructions sysvar. |

Args: none.

Checks: settlement `Unpaid`; `settlement.sequence == app.paid_count` (settlements are marked paid in order); the preceding instruction attests the paid digest.

Effect: settlement `Paid`; `paid_count += 1`; `unpaid_total -= total`. Emits `SettlementPaid`.

Errors: `AlreadyPaid`, `OutOfOrder`, `BadAttestation`, `Overflow`.

### 3.6 SDK-generated: `process_undelegation`

Added by the ephemeral-rollups SDK's `#[ephemeral]` attribute. The delegation program calls it to hand an undelegated account (the pot) back to the ledger. Apps do not call it. Accounts in IDL order: `base_account` (W), `buffer` (PDA `["undelegate-buffer", base_account]` under the delegation program), `payer` (W), `system_program`. Args: `account_seeds: Vec<Vec<u8>>`. Its behavior is defined by the SDK, not by ledger code.

---

## 4. Events

Emitted with Anchor `emit!` (program log). In most events `app` is the app's **program ID**. In `ExitRequested` it is the **App PDA**, because that event takes `Balance.app`.

| Event | Fields | Emitted by |
|---|---|---|
| `ConfigChanged` | `admin: Pubkey`, `attestor: Pubkey`, `paused: bool` (values after the change) | `set_config` |
| `AppRegistered` | `app: Pubkey` (program ID), `authority: Pubkey` | `register_app` |
| `AppActivating` | `app: Pubkey`, `vault_address: [u8; 43]`, `genesis_head: [u8; 32]`, `activates_at: i64` | `activate_app` |
| `ActivationCancelled` | `app: Pubkey`, `guardian: Pubkey` | `freeze_app` on an `Activating` app |
| `AppFrozen` | `app: Pubkey`, `guardian: Pubkey` | `freeze_app` on an `Active` app |
| `AppUnfrozen` | `app: Pubkey` | `unfreeze_app` |
| `DepositCredited` | `app: Pubkey`, `deposit_id: [u8; 32]`, `index: u64`, `amount: u64` (gross), `fee: u64`, `session_key: Pubkey` | `credit_deposit` |
| `DepositVoided` | `app: Pubkey`, `deposit_id: [u8; 32]` | `void_deposit` |
| `PotOpened` | `app: Pubkey`, `pot: Pubkey`, `open_deadline: i64` | `open_pot` |
| `BoughtIn` | `pot: Pubkey`, `player: Pubkey`, `seat: u8`, `amount: u64` (this buy-in only) | `buy_in` |
| `LeftPot` | `pot: Pubkey`, `player: Pubkey`, `amount: u64` | `leave_pot` |
| `PotDelegated` | `pot: Pubkey`, `session_deadline: i64`, `total: u64` | `delegate_pot` |
| `PotCashedOut` | `pot: Pubkey`, `total: u64` | `cash_out` |
| `PotRefunded` | `pot: Pubkey`, `total: u64` | `refund_pot` |
| `ExitRequested` | `app: Pubkey` (App PDA), `session_key: Pubkey`, `amount: u64` (balance at request time) | `request_exit` |
| `Settled` | `app: Pubkey`, `sequence: u64`, `count: u8`, `total: u64` | `settle` |
| `SettlementPaid` | `app: Pubkey`, `sequence: u64` | `mark_paid` |

`initialize`, `set_guardians`, `set_cap`, `move_seat` and `close_pot` emit no event.

---

## 5. Errors

From [`error.rs`](../../programs/zol-ledger/src/error.rs), enum `ZolError`. Code is `6000 + index`.

| Code | Name | Message |
|---|---|---|
| 6000 | `FeeTooHigh` | Fee rate or allowance above the program's constant cap |
| 6001 | `BadParams` | Parameters out of range |
| 6002 | `BadGuardians` | Guardian set or threshold invalid |
| 6003 | `GuardianThreshold` | Not enough guardian signatures |
| 6004 | `NotGuardian` | Signer is not a guardian |
| 6005 | `NotUpgradeAuthority` | Only the program upgrade authority can initialize |
| 6006 | `Paused` | Ledger is paused |
| 6007 | `AppFrozen` | App is frozen |
| 6008 | `AppNotActive` | App is not active |
| 6009 | `BadAppState` | App is in the wrong state for this instruction |
| 6010 | `NotAProgram` | Account is not an executable program |
| 6011 | `BadAppAuthority` | App authority is not the app program's zol-app address |
| 6012 | `BadAttestation` | Previous instruction is not a matching Ed25519 attestation |
| 6013 | `OverCap` | Deposit would exceed the app's cap |
| 6014 | `PayoutCommitmentMismatch` | Payout commitment differs from this balance's |
| 6015 | `ZeroAmount` | Amount must be positive |
| 6016 | `Overflow` | Arithmetic overflow |
| 6017 | `InsufficientBalance` | Insufficient balance |
| 6018 | `BalanceExiting` | Balance is marked for exit |
| 6019 | `BadPotState` | Pot is in the wrong state |
| 6020 | `BadDeadline` | Deadline out of range |
| 6021 | `OpenDeadlinePassed` | Open deadline has passed |
| 6022 | `PotFull` | Pot is full |
| 6023 | `NoSeat` | Player has no seat in this pot |
| 6024 | `ReceiptMismatch` | Expected players and buy-ins differ from the receipt |
| 6025 | `PotNotReturned` | Pot has not returned from the rollup |
| 6026 | `PotInvalid` | Pot does not match its receipt |
| 6027 | `BadSeat` | Seat index out of range |
| 6028 | `RefundNotAllowed` | Refund conditions not met |
| 6029 | `BadAccount` | Wrong account passed |
| 6030 | `DuplicateAccount` | Duplicate account |
| 6031 | `BelowMinExit` | Balance below the minimum exit |
| 6032 | `NotExiting` | Balance is not marked for exit |
| 6033 | `BadPayoutCount` | Nothing to settle, or too many payouts |
| 6034 | `OutOfOrder` | Settlement is not the next unpaid one |
| 6035 | `AlreadyPaid` | Settlement already paid |
| 6036 | `AppDataTooLong` | App data too long |
| 6037 | `BadFlags` | Unknown deposit flags |
| 6038 | `BadDenomination` | Deposit amount is not a permitted denomination |
| 6039 | `BadAction` | A pot's return action must call its own app, within the limits, with a signing escrow authority |

---

## 6. Invariants the ledger guarantees

These follow from the code and its doc comments. They hold for any app program, since apps are untrusted callers.

**Value**

- Value enters only through `credit_deposit`, which needs an attestor signature over the exact deposit, a permitted denomination, and room under the app's `deposit_cap`. A deposit ID is credited or voided at most once, never both.
- Value leaves only through `settle`, into a numbered settlement record that the operators pay on Zcash. The exit fee and network fee allowance go to `fee_pool`.
- No pot instruction creates value. `buy_in` and `leave_pot` move value between a balance and a seat. `move_seat` moves value between two seats of one pot. `cash_out` pays out only a pot whose seat amounts sum to the receipt's total. `refund_pot` pays back exactly the recorded buy-ins.
- `App.liability` equals the sum of the app's balances plus the totals of its live receipts. Credits add the net amount; settlements subtract the settled balances; pot instructions leave it unchanged.
- Credits are refused once `liability + unpaid_total` would exceed `deposit_cap`. A new app's cap is 0.

**Who can move what**

- Only an app's program can act as that app: its authority is its own `["zol-app"]` PDA, and registration needs that PDA's signature.
- An app touches only its own pots. `open_pot`, `buy_in` and `delegate_pot` require `App.authority`; `move_seat` and `close_pot` require the pot's `app_authority`.
- Nobody enters a pot without both the app's and the player's signatures (`buy_in`).
- A player can leave an `Open` pot alone, in any app state.
- Payouts go only to the receipt's players. `cash_out` and `refund_pot` credit seat `i` to the balance whose session key is `players[i]`, in the same app. Player keys live only in the receipt, which never leaves Solana.
- A return action from `close_pot` may call only the pot's own app, within fixed size and compute limits, signed only by the app's escrow. The ledger never signs for it.
- An exiting balance cannot buy in. A balance's payout commitment is fixed by its first credit; settlement pays to it.

**Liveness and deadlines**

- An `Open` pot that did not start by its `open_deadline` can be refunded by anyone.
- A delegated pot that has not validly returned by `session_deadline + refund_grace_secs` can be refunded by anyone. `refund_grace_secs` is at least 3,900 s in release builds, longer than a forced undelegation (about 60 minutes). A pot that did validly return can only be cashed out.
- `leave_pot`, `move_seat`, `close_pot`, `cash_out`, `refund_pot`, `request_exit`, `void_deposit` and `mark_paid` work when the app is frozen or the ledger is paused, so pots in the rollup can still come home and players are not trapped.
- The pot's rollup validator is fixed at delegation and recorded in the receipt. The delegation program accepts commits only from it.

**Governance limits**

- The admin can set parameters only within the program's caps: each fee at most 1%, network fee allowance at most 0.001 ZEC, at most 16 seats, and the activation-delay and refund-grace floors.
- The admin cannot change guardians or unfreeze an app.
- An app's vault address and genesis head take effect only after the activation delay (at least 1 hour in release builds). Any one guardian can cancel during the delay. No instruction changes them once the app is active.
- Any one guardian can freeze an active app. Unfreezing and replacing the guardian set need a threshold of guardians.
- Settlements are numbered per app and marked paid strictly in order, each with an attestation.
