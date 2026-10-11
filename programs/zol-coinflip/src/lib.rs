//! zol-coinflip: the flagship ZOL app. A two-player coin flip on a ledger pot.
//!
//! | Instruction    | Runs on | What it does |
//! |----------------|---------|--------------|
//! | `register`     | Solana  | One-off: registers this program with the ledger (CPI `register_app`) |
//! | `create_table` | Solana  | Opens a ledger pot (CPI `open_pot`) and a `Table` for it, naming the rollup validator |
//! | `join`         | Solana  | CPI `buy_in` for the table stake; the player's session key signs |
//! | `join_rounds`  | Solana  | `join` for `rounds` stakes at once: a session of up to `rounds` flips |
//! | `start`        | Solana  | CPI `delegate_pot` with the receipt's stakes, and delegates the table, both to the table's validator |
//! | `flip`         | Rollup  | A seated player requests MagicBlock VRF randomness; the callback is `land`. Again after each landing while both seats cover a stake |
//! | `land`         | Rollup  | VRF callback only: CPI `move_seat` of one stake from the loser's seat to the winner's |
//! | `finish`       | Rollup  | The creator or a seated player (anyone after the session deadline): CPI `close_pot`, commits and undelegates the table |
//! | `finish_and_pay` | Rollup | `finish`, and the table's return schedules `cash_out_table` on Solana (a Magic Action) |
//! | `crank_close`  | Rollup  | No signer (a scheduled task): finishes and pays a table that is idle, short or near its deadline |
//! | `schedule_closer` | Rollup | Anyone: schedules `crank_close` for a table until its session deadline |
//! | `cash_out_table` | Solana | Anyone (and the Magic Action): ledger `cash_out` for both seats, then closes the table |
//! | `close_table`  | Solana  | Anyone, once the ledger's receipt is terminal: closes the table, rent to its creator |
//!
//! `cash_out` is the ledger's and anyone may call it once the pot is back.
//!
//! Who sits where is the ledger's `PotReceipt`, the source of truth. `start` copies the two
//! keys into the table only so the rollup, where the receipt is not, can tell who may flip
//! and finish. The app signs every ledger call with its `["zol-app"]` address.

use anchor_lang::prelude::*;
use anchor_lang::{InstructionData, ToAccountMetas};
use ephemeral_rollups_sdk::anchor::{commit, delegate, ephemeral, vrf, vrf_callback};
use ephemeral_rollups_sdk::cpi::cpi_delegate;
use ephemeral_rollups_sdk::types::DelegateAccountArgs;
use ephemeral_rollups_sdk::utils::{close_pda_with_system_transfer, create_pda};
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;
#[allow(deprecated)]
use ephemeral_rollups_sdk::crank::{ScheduleCrankCpi, ScheduleTaskArgs};
use ephemeral_rollups_sdk::ephem::{CallHandler, FoldableIntentBuilder, MagicIntentBundleBuilder};
use ephemeral_rollups_sdk::{ActionArgs, ShortAccountMeta};
use ephemeral_rollups_sdk::vrf::instructions::{create_request_randomness_ix, RequestRandomnessParams};
use ephemeral_rollups_sdk::vrf::types::SerializableAccountMeta;
use zol_ledger::state::{bps, Config, Pot, PotReceipt, PotState, SeatStake};

declare_id!("7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo");

pub const APP_AUTHORITY_SEED: &[u8] = b"zol-app";
pub const TABLE_SEED: &[u8] = b"table";
pub const SEATS: u8 = 2;
pub const NO_WINNER: u8 = u8::MAX;

/// A deposit also splits into this many smaller stakes.
pub const STAKE_SPLIT: u64 = 10;
/// The most stakes one seat may buy into a table (`join_rounds`).
pub const MAX_ROUNDS: u16 = 100;
/// A flip whose VRF answer has not landed after this long may be abandoned by `finish`.
pub const FLIP_TIMEOUT_SECS: i64 = 60;
/// A table with no flip for this long may be closed by anyone's `crank_close`.
pub const IDLE_SECS: i64 = 120;
/// `crank_close` closes a table this long before its session deadline.
pub const DEADLINE_MARGIN_SECS: i64 = 300;
/// Lamports `start` adds to a table above its rent, for the fees it pays as a delegated
/// payer in `crank_close` (returned to the creator on close).
pub const TABLE_FEE_BUFFER: u64 = 1_000_000;
/// How often the scheduled `crank_close` looks at a table (devnet ran it about every 5 s
/// when 20 s was asked, so iterations are sized for the faster pace).
pub const CLOSER_INTERVAL_MS: i64 = 5_000;

/// The stakes `create_table` accepts: for each denomination `d` the ledger permits
/// (`Config.params.denominations`, nonzero entries), one deposit's net credited value
/// `n = d - floor(d * entry_fee_bps / 10_000)`, and a tenth of it, `floor(n / STAKE_SPLIT)`.
/// Whole-deposit stakes come first, in denomination order.
pub fn allowed_stakes(params: &zol_ledger::state::Params) -> Vec<u64> {
    let net: Vec<u64> = params.denominations.iter().filter(|d| **d > 0).map(|d| d - bps(*d, params.entry_fee_bps)).collect();
    let split = net.iter().map(|n| n / STAKE_SPLIT).filter(|s| *s > 0);
    let mut out: Vec<u64> = Vec::new();
    for s in net.iter().copied().chain(split) {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// System-owned PDA that pays the table's delegation deposit, so that this program can
/// later sign the delegation program's owner-requested undelegation for a stuck table
/// (`rescue_table`). `start` tops it up from its payer; undelegation refunds return here.
pub const RENT_VAULT_SEED: &[u8] = b"rent-vault";
/// `start` tops the vault up to this balance (buffer rent while delegating, the
/// delegation record and metadata deposit, an undelegation request).
pub const RENT_VAULT_TARGET: u64 = 10_000_000;

/// Oracle queues `flip` accepts: MagicBlock's ephemeral (rollup) queue and its local/test
/// twin, both from ephemeral-vrf-sdk. Pinning them stops a caller from naming a queue
/// served by an oracle they run, which would let them pick the winner.
pub const ALLOWED_QUEUES: [Pubkey; 2] = [
    ephemeral_rollups_sdk::vrf::consts::DEFAULT_EPHEMERAL_QUEUE,
    ephemeral_rollups_sdk::vrf::consts::DEFAULT_EPHEMERAL_TEST_QUEUE,
];

#[ephemeral]
#[program]
pub mod zol_coinflip {
    use super::*;

    /// Registers this app with the ledger. Anyone may pay; the ledger makes it
    /// `Pending` until the admin activates it.
    pub fn register(ctx: Context<Register>) -> Result<()> {
        let a = &ctx.accounts;
        let bump = ctx.bumps.app_authority;
        zol_ledger::cpi::register_app(CpiContext::new_with_signer(
            zol_ledger::ID,
            zol_ledger::cpi::accounts::RegisterApp {
                payer: a.payer.to_account_info(),
                config: a.config.to_account_info(),
                app_program: a.this_program.to_account_info(),
                app_authority: a.app_authority.to_account_info(),
                app: a.app.to_account_info(),
                system_program: a.system_program.to_account_info(),
            },
            &[&[APP_AUTHORITY_SEED, &[bump]]],
        ))
    }

    /// The validator is fixed here, so players see which rollup will decide their pot
    /// before they join, and nobody can later start the table towards another one.
    pub fn create_table(ctx: Context<CreateTable>, pot_id: [u8; 32], stake: u64, open_secs: i64, validator: Pubkey) -> Result<()> {
        require!(stake > 0, CoinflipError::ZeroStake);
        require_keys_neq!(validator, Pubkey::default(), CoinflipError::BadValidator);
        let params = {
            let info = &ctx.accounts.config;
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let (expected, _) = Pubkey::find_program_address(&[zol_ledger::state::seeds::CONFIG], &zol_ledger::ID);
            require_keys_eq!(info.key(), expected, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            Config::try_deserialize(&mut &data[..])?.params
        };
        require!(allowed_stakes(&params).contains(&stake), CoinflipError::BadStake);
        let now = Clock::get()?.unix_timestamp;
        let open_deadline = now.checked_add(open_secs).ok_or(CoinflipError::Overflow)?;
        let a = &ctx.accounts;
        let bump = ctx.bumps.app_authority;
        zol_ledger::cpi::open_pot(
            CpiContext::new_with_signer(
                zol_ledger::ID,
                zol_ledger::cpi::accounts::OpenPot {
                    payer: a.payer.to_account_info(),
                    config: a.config.to_account_info(),
                    app: a.app.to_account_info(),
                    authority: a.app_authority.to_account_info(),
                    pot: a.pot.to_account_info(),
                    receipt: a.receipt.to_account_info(),
                    system_program: a.system_program.to_account_info(),
                },
                &[&[APP_AUTHORITY_SEED, &[bump]]],
            ),
            pot_id,
            open_deadline,
        )?;
        let t = &mut ctx.accounts.table;
        t.pot = ctx.accounts.pot.key();
        t.pot_id = pot_id;
        t.stake = stake;
        t.validator = validator;
        t.creator = ctx.accounts.payer.key();
        t.state = TableState::Open;
        t.winner = NO_WINNER;
        t.randomness = [0; 32];
        t.bump = ctx.bumps.table;
        t.flips = 0;
        t.flip_requested_at = 0;
        t.players = [Pubkey::default(); 2];
        t.session_deadline = 0;
        t.results = 0;
        emit!(TableCreated { table: t.key(), pot: t.pot, stake, validator });
        Ok(())
    }

    /// The player's session key signs; the ledger moves the stake from their balance.
    pub fn join(ctx: Context<Join>) -> Result<()> {
        join_for(ctx, 1)
    }

    /// `join` for `rounds` stakes at once (1..=`MAX_ROUNDS`): the seat can play up to
    /// that many flips in one rollup session. Both seats must buy the same number.
    pub fn join_rounds(ctx: Context<Join>, rounds: u16) -> Result<()> {
        join_for(ctx, rounds)
    }

    /// Anyone, once both seats hold the stake. Sends the pot and the table to the
    /// table's validator.
    pub fn start(ctx: Context<Start>, session_secs: i64) -> Result<()> {
        start_table(ctx, session_secs)
    }

    /// Rollup. A seated player flips a started table, and again after each landing while
    /// both seats still cover a stake; the outcome is the oracle's.
    pub fn flip(ctx: Context<Flip>, caller_seed: [u8; 32]) -> Result<()> {
        require!(matches!(ctx.accounts.table.state, TableState::Started | TableState::Landed), CoinflipError::BadTableState);
        require!(ctx.accounts.table.players.contains(&ctx.accounts.payer.key()), CoinflipError::NotAtTable);
        require!(ALLOWED_QUEUES.contains(&ctx.accounts.oracle_queue.key()), CoinflipError::BadQueue);
        {
            let info = ctx.accounts.pot.to_account_info();
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            let pot = Pot::try_deserialize(&mut &data[..])?;
            let stake = ctx.accounts.table.stake;
            require!(pot.seat_count == SEATS && pot.amounts[..SEATS as usize].iter().all(|a| *a >= stake), CoinflipError::SeatShort);
        }
        ctx.accounts.table.state = TableState::Flipping;
        ctx.accounts.table.flip_requested_at = Clock::get()?.unix_timestamp;
        let ix = create_request_randomness_ix(RequestRandomnessParams {
            payer: ctx.accounts.payer.key(),
            oracle_queue: ctx.accounts.oracle_queue.key(),
            callback_program_id: crate::ID,
            callback_discriminator: instruction::Land::DISCRIMINATOR.to_vec(),
            caller_seed,
            accounts_metas: Some(vec![
                SerializableAccountMeta { pubkey: ctx.accounts.table.key(), is_signer: false, is_writable: true },
                SerializableAccountMeta { pubkey: ctx.accounts.table.pot, is_signer: false, is_writable: true },
                SerializableAccountMeta { pubkey: app_authority_address(), is_signer: false, is_writable: false },
                SerializableAccountMeta { pubkey: zol_ledger::ID, is_signer: false, is_writable: false },
            ]),
            ..Default::default()
        });
        ctx.accounts.invoke_signed_vrf(&ctx.accounts.payer.to_account_info(), &ix)?;
        emit!(FlipRequested { table: ctx.accounts.table.key() });
        Ok(())
    }

    /// Rollup, VRF callback only (the scoped VRF identity must sign). The winner's
    /// seat takes the loser's stake.
    pub fn land(ctx: Context<Land>, randomness: [u8; 32]) -> Result<()> {
        require!(ctx.accounts.table.state == TableState::Flipping, CoinflipError::BadTableState);
        let pot = {
            let info = ctx.accounts.pot.to_account_info();
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            Pot::try_deserialize(&mut &data[..])?
        };
        require!(pot.seat_count == SEATS, CoinflipError::NotFull);
        let winner = outcome(&randomness);
        let loser = 1 - winner;
        // One stake per flip (`flip` checked both seats cover it).
        let amount = ctx.accounts.table.stake.min(pot.amounts[loser as usize]);
        if amount > 0 {
            let a = &ctx.accounts;
            let bump = ctx.bumps.app_authority;
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
        }
        let t = &mut ctx.accounts.table;
        t.state = TableState::Landed;
        t.winner = winner;
        t.randomness = randomness;
        t.flips = t.flips.saturating_add(1);
        t.results = (t.results << 1) | winner as u64;
        emit!(Landed { table: t.key(), winner, amount });
        Ok(())
    }

    /// Rollup. The table's creator or a seated player, between flips (or before the first):
    /// returns the pot (ledger `close_pot`) and the table to Solana. Anyone may once the
    /// session deadline has passed, so the pot comes home with its result before the ledger
    /// would refund the buy-ins. A flip whose VRF answer has not landed after
    /// `FLIP_TIMEOUT_SECS` may be abandoned; its stakes stay where they were.
    pub fn finish(ctx: Context<Finish>) -> Result<()> {
        check_finish(&ctx.accounts.table, ctx.accounts.payer.key(), Clock::get()?.unix_timestamp)?;
        let bump = ctx.bumps.app_authority;
        let a = ctx.accounts;
        close_in_rollup(&a.payer.to_account_info(), None, &mut a.table, &a.pot, &a.app_authority, bump, &a.magic_program, &a.magic_context, None, None)
    }

    /// Rollup. `finish` with the same rules, and the table's return to Solana schedules
    /// `cash_out_table` there (a Magic Action paid from the rent vault's escrow), so both
    /// seats reach the players' balances without anyone polling for the pot.
    pub fn finish_and_pay(ctx: Context<FinishAndPay>) -> Result<()> {
        check_finish(&ctx.accounts.table, ctx.accounts.payer.key(), Clock::get()?.unix_timestamp)?;
        let (bump, vault_bump) = (ctx.bumps.app_authority, ctx.bumps.rent_vault);
        let a = ctx.accounts;
        close_in_rollup(&a.payer.to_account_info(), None, &mut a.table, &a.pot, &a.app_authority, bump, &a.magic_program, &a.magic_context, None, Some((&a.rent_vault, vault_bump)))
    }

    /// Rollup, no signer: a scheduled task (`schedule_closer`) runs it. Finishes and pays a
    /// table nobody can or will play any more: no flip for `IDLE_SECS`, a seat that cannot
    /// cover a stake, or within `DEADLINE_MARGIN_SECS` of the session deadline. Otherwise it
    /// does nothing, so repeated runs are harmless. The table signs as the commit's payer.
    pub fn crank_close(ctx: Context<CrankClose>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let t = &ctx.accounts.table;
        let stale_flip = t.state == TableState::Flipping && now >= t.flip_requested_at.saturating_add(FLIP_TIMEOUT_SECS);
        if !(matches!(t.state, TableState::Started | TableState::Landed) || stale_flip) {
            return Ok(());
        }
        let short = {
            let info = ctx.accounts.pot.to_account_info();
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            let pot = Pot::try_deserialize(&mut &data[..])?;
            pot.amounts[..SEATS as usize].iter().any(|a| *a < t.stake)
        };
        let idle = now >= t.flip_requested_at.saturating_add(IDLE_SECS) && t.state != TableState::Flipping;
        let late = now >= t.session_deadline.saturating_sub(DEADLINE_MARGIN_SECS);
        if !(idle || short || late || stale_flip) {
            return Ok(());
        }
        let (bump, vault_bump) = (ctx.bumps.app_authority, ctx.bumps.rent_vault);
        let table_bump = t.bump;
        let pot_id = t.pot_id;
        let a = ctx.accounts;
        let payer = a.table.to_account_info();
        let table_seeds: &[&[u8]] = &[TABLE_SEED, &pot_id, &[table_bump]];
        require_keys_eq!(a.magic_fee_vault.key(), magic_fee_vault(&a.table.validator), CoinflipError::BadAccount);
        let fee_vault = a.magic_fee_vault.to_account_info();
        close_in_rollup(&payer, Some(table_seeds), &mut a.table, &a.pot, &a.app_authority, bump, &a.magic_program, &a.magic_context, Some(&fee_vault), Some((&a.rent_vault, vault_bump)))
    }

    /// Rollup, anyone: schedules `crank_close` for this table every `CLOSER_INTERVAL_MS`
    /// until its session deadline, so the table comes home even if every client is gone.
    #[allow(deprecated)] // the legacy ScheduleTask; the validator turns it into a hydra crank
    pub fn schedule_closer<'info>(ctx: Context<'info, ScheduleCloser<'info>>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let t = &ctx.accounts.table;
        require!(matches!(t.state, TableState::Started | TableState::Flipping | TableState::Landed), CoinflipError::BadTableState);
        let left_ms = t.session_deadline.saturating_sub(now).max(0).saturating_mul(1000);
        let iterations = left_ms / CLOSER_INTERVAL_MS + 1;
        let a = &ctx.accounts;
        let ix = Instruction {
            program_id: crate::ID,
            accounts: accounts::CrankClose {
                table: a.table.key(),
                pot: a.pot.key(),
                app_authority: a.app_authority.key(),
                ledger_program: a.ledger_program.key(),
                rent_vault: a.rent_vault.key(),
                magic_fee_vault: magic_fee_vault(&t.validator),
                magic_program: a.magic_program.key(),
                magic_context: a.magic_context.key(),
            }
            .to_account_metas(None),
            data: instruction::CrankClose {}.data(),
        };
        let task_id = i64::from_le_bytes(t.pot_id[..8].try_into().unwrap()) & i64::MAX;
        ScheduleCrankCpi {
            payer: &ctx.accounts.payer,
            magic_program: &ctx.accounts.magic_program,
            instruction_accounts: &[
                a.table.to_account_info(),
                a.pot.to_account_info(),
                a.app_authority.to_account_info(),
                a.ledger_program.to_account_info(),
                a.rent_vault.to_account_info(),
                a.magic_fee_vault.to_account_info(),
                a.magic_context.to_account_info(),
            ],
            args: ScheduleTaskArgs { task_id, execution_interval_millis: CLOSER_INTERVAL_MS, iterations, instructions: vec![ix] },
        }
        .invoke()?;
        emit!(CloserScheduled { table: a.table.key(), iterations });
        Ok(())
    }

    /// Solana, anyone; also the Magic Action `finish_and_pay` schedules. Once the pot is
    /// back: ledger `cash_out` for both seats, then the table closes with its rent to the
    /// creator. If the pot is not back yet it does nothing (a keeper or a later call pays),
    /// so a timing race never fails the action. Extra trailing accounts (the action's
    /// escrow) are ignored.
    pub fn cash_out_table<'info>(ctx: Context<'info, CashOutTable<'info>>) -> Result<()> {
        let a = &ctx.accounts;
        let read_receipt = || -> Result<PotReceipt> {
            let info = &a.receipt;
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let (expected, _) = Pubkey::find_program_address(&[zol_ledger::state::seeds::RECEIPT, a.pot.key().as_ref()], &zol_ledger::ID);
            require_keys_eq!(info.key(), expected, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            Ok(PotReceipt::try_deserialize(&mut &data[..])?)
        };
        let r = read_receipt()?;
        require_keys_eq!(a.table.key(), table_address(&r.pot_id), CoinflipError::BadAccount);
        if r.state == PotState::Delegated {
            if *a.pot.owner != zol_ledger::ID {
                msg!("pot not back on Solana yet");
                return Ok(());
            }
            zol_ledger::cpi::cash_out(
                CpiContext::new(
                    zol_ledger::ID,
                    zol_ledger::cpi::accounts::PotOutcome {
                        config: a.config.to_account_info(),
                        app: a.app.to_account_info(),
                        receipt: a.receipt.to_account_info(),
                        pot: a.pot.to_account_info(),
                    },
                )
                .with_remaining_accounts(vec![a.balance0.to_account_info(), a.balance1.to_account_info()]),
            )?;
        }
        let r = read_receipt()?;
        let table = a.table.to_account_info();
        if matches!(r.state, PotState::CashedOut | PotState::Refunded) && *table.owner == crate::ID {
            let creator = {
                let data = table.try_borrow_data()?;
                Table::try_deserialize(&mut &data[..])?.creator
            };
            require_keys_eq!(creator, a.creator.key(), CoinflipError::BadAccount);
            let lamports = table.lamports();
            **a.creator.to_account_info().try_borrow_mut_lamports()? += lamports;
            **table.try_borrow_mut_lamports()? = 0;
            table.assign(&anchor_lang::system_program::ID);
            table.resize(0)?;
            emit!(TableClosed { table: table.key(), creator, pot_state: r.state as u8 });
        }
        Ok(())
    }

    /// Solana, anyone. Once the ledger's receipt for the table's pot is terminal
    /// (cashed out or refunded), closes the table and returns its rent to the creator.
    /// A delegated table (owned by the delegation program) fails the `Account<Table>`
    /// owner check, so only a table that is back on Solana, or never left, can close.
    pub fn close_table(ctx: Context<CloseTable>) -> Result<()> {
        let info = &ctx.accounts.receipt;
        require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
        let (expected, _) = Pubkey::find_program_address(
            &[zol_ledger::state::seeds::RECEIPT, ctx.accounts.table.pot.as_ref()],
            &zol_ledger::ID,
        );
        require_keys_eq!(info.key(), expected, CoinflipError::BadAccount);
        let r = {
            let data = info.try_borrow_data()?;
            PotReceipt::try_deserialize(&mut &data[..])?
        };
        require!(matches!(r.state, PotState::CashedOut | PotState::Refunded), CoinflipError::PotLive);
        emit!(TableClosed { table: ctx.accounts.table.key(), creator: ctx.accounts.creator.key(), pot_state: r.state as u8 });
        Ok(())
    }

    /// Solana, anyone. Rescues a table stuck in the rollup (the VRF never answered, or
    /// nobody finished) once the ledger has refunded its pot. It uses the delegation
    /// program's owner-requested undelegation, which exists on MagicBlock's devnet
    /// delegation program only (spike 0.9):
    ///
    /// 1. First call: `RequestUndelegation` (ix 26), signed with the table's seeds and the
    ///    rent vault (the table's delegation rent payer). An honest validator then
    ///    returns the table by itself within about a second.
    /// 2. If it does not, a second call after the request's timeout (9,000 slots, about an
    ///    hour) runs `UndelegateWithRollbackAfterTimeout` (ix 27): the table comes back to
    ///    this program with its last committed state.
    ///
    /// Then `close_table` returns the rent. Refused unless the receipt is `Refunded`, so a
    /// live game can never be pulled out of its rollup.
    pub fn rescue_table(ctx: Context<RescueTable>) -> Result<()> {
        let a = &ctx.accounts;
        let info = a.table.to_account_info();
        require_keys_eq!(*info.owner, ephemeral_rollups_sdk::id(), CoinflipError::NotDelegated);
        // While delegated, the Solana copy holds the last committed bytes.
        let saved: Vec<u8> = info.try_borrow_data()?.to_vec();
        let table = Table::try_deserialize(&mut &saved[..])?;
        let (expected, table_bump) = Pubkey::find_program_address(&[TABLE_SEED, &table.pot_id], &crate::ID);
        require_keys_eq!(expected, info.key(), CoinflipError::BadAccount);
        require_keys_eq!(a.validator.key(), table.validator, CoinflipError::BadAccount);
        let r = {
            let rinfo = &a.receipt;
            require_keys_eq!(*rinfo.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let (rk, _) = Pubkey::find_program_address(&[zol_ledger::state::seeds::RECEIPT, table.pot.as_ref()], &zol_ledger::ID);
            require_keys_eq!(rinfo.key(), rk, CoinflipError::BadAccount);
            let data = rinfo.try_borrow_data()?;
            PotReceipt::try_deserialize(&mut &data[..])?
        };
        require!(r.state == PotState::Refunded, CoinflipError::NotRefunded);

        let table_seeds: &[&[u8]] = &[TABLE_SEED, &table.pot_id, &[table_bump]];
        let vault_seeds: &[&[u8]] = &[RENT_VAULT_SEED, &[ctx.bumps.rent_vault]];
        let dlp = ephemeral_rollups_sdk::id();
        if a.undelegation_request.lamports() == 0 {
            let ix = Instruction {
                program_id: dlp,
                accounts: vec![
                    AccountMeta::new(a.rent_vault.key(), true),
                    AccountMeta::new_readonly(info.key(), true),
                    AccountMeta::new_readonly(crate::ID, false),
                    AccountMeta::new(a.undelegation_request.key(), false),
                    AccountMeta::new_readonly(a.delegation_record.key(), false),
                    AccountMeta::new(a.delegation_metadata.key(), false),
                    AccountMeta::new_readonly(a.system_program.key(), false),
                ],
                data: 26u64.to_le_bytes().to_vec(),
            };
            invoke_signed(
                &ix,
                &[
                    a.rent_vault.to_account_info(),
                    info.clone(),
                    a.this_program.to_account_info(),
                    a.undelegation_request.to_account_info(),
                    a.delegation_record.to_account_info(),
                    a.delegation_metadata.to_account_info(),
                    a.system_program.to_account_info(),
                ],
                &[table_seeds, vault_seeds],
            )?;
            emit!(RescueRequested { table: info.key() });
            return Ok(());
        }
        let ix = Instruction {
            program_id: dlp,
            accounts: vec![
                AccountMeta::new(info.key(), true),
                AccountMeta::new_readonly(crate::ID, false),
                AccountMeta::new(a.undelegation_request.key(), false),
                AccountMeta::new(a.delegation_record.key(), false),
                AccountMeta::new(a.delegation_metadata.key(), false),
                AccountMeta::new(a.rent_vault.key(), false),
                AccountMeta::new(a.commit_state.key(), false),
                AccountMeta::new(a.commit_record.key(), false),
                AccountMeta::new(a.validator.key(), false),
            ],
            data: 27u64.to_le_bytes().to_vec(),
        };
        invoke_signed(
            &ix,
            &[
                info.clone(),
                a.this_program.to_account_info(),
                a.undelegation_request.to_account_info(),
                a.delegation_record.to_account_info(),
                a.delegation_metadata.to_account_info(),
                a.rent_vault.to_account_info(),
                a.commit_state.to_account_info(),
                a.commit_record.to_account_info(),
                a.validator.to_account_info(),
            ],
            &[table_seeds],
        )?;
        // The delegation program hands the account back empty; restore the last committed bytes.
        require_keys_eq!(*info.owner, crate::ID, CoinflipError::NotDelegated);
        info.resize(saved.len())?;
        info.try_borrow_mut_data()?.copy_from_slice(&saved);
        emit!(TableRescued { table: info.key() });
        Ok(())
    }
}

fn join_for(ctx: Context<Join>, rounds: u16) -> Result<()> {
    require!(rounds >= 1 && rounds <= MAX_ROUNDS, CoinflipError::BadRounds);
    let a = &ctx.accounts;
    require!(a.table.state == TableState::Open, CoinflipError::BadTableState);
    let r = load_receipt(&a.receipt, &a.table)?;
    let player = a.session_key.key();
    require!(r.seat_of(&player).is_none(), CoinflipError::AlreadySeated);
    require!(r.seat_count < SEATS, CoinflipError::TableFull);
    let bump = ctx.bumps.app_authority;
    zol_ledger::cpi::buy_in(
        CpiContext::new_with_signer(
            zol_ledger::ID,
            zol_ledger::cpi::accounts::BuyIn {
                config: a.config.to_account_info(),
                app: a.app.to_account_info(),
                authority: a.app_authority.to_account_info(),
                session_key: a.session_key.to_account_info(),
                balance: a.balance.to_account_info(),
                pot: a.pot.to_account_info(),
                receipt: a.receipt.to_account_info(),
            },
            &[&[APP_AUTHORITY_SEED, &[bump]]],
        ),
        a.table.stake.checked_mul(rounds as u64).ok_or(CoinflipError::Overflow)?,
    )?;
    emit!(Joined { table: a.table.key(), player, seat: r.seat_count });
    Ok(())
}

fn start_table(ctx: Context<Start>, session_secs: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let table_info = ctx.accounts.table.to_account_info();
    require_keys_eq!(*table_info.owner, crate::ID, CoinflipError::BadAccount);
    let mut table = {
        let data = table_info.try_borrow_data()?;
        Table::try_deserialize(&mut &data[..])?
    };
    let (expected_table, _) = Pubkey::find_program_address(&[TABLE_SEED, &table.pot_id], &crate::ID);
    require_keys_eq!(expected_table, table_info.key(), CoinflipError::BadAccount);
    require_keys_eq!(table.pot, ctx.accounts.pot.key(), CoinflipError::BadAccount);
    require!(table.state == TableState::Open, CoinflipError::BadTableState);
    let validator = table.validator;

    // The receipt, not the app, says who is playing.
    let r = load_receipt(&ctx.accounts.receipt, &table)?;
    require!(r.state == PotState::Open, CoinflipError::BadTableState);
    require!(r.seat_count == SEATS, CoinflipError::NotFull);
    let expected: Vec<SeatStake> = (0..SEATS as usize)
        .map(|i| SeatStake { player: r.players[i], amount: r.buy_ins[i] })
        .collect();
    // Equal seats, each a whole number of stakes: every flip moves exactly one stake.
    let first = expected[0].amount;
    require!(first > 0 && first % table.stake == 0 && expected.iter().all(|s| s.amount == first), CoinflipError::BadStake);

    // Write the table before it changes owner. The rollup cannot read the receipt, so the
    // table carries the seated keys and the deadline for `flip` and `finish`.
    let session_deadline = now.checked_add(session_secs).ok_or(CoinflipError::Overflow)?;
    table.state = TableState::Started;
    table.players = [r.players[0], r.players[1]];
    table.session_deadline = session_deadline;
    table.flip_requested_at = now; // last activity: `crank_close` counts idleness from here
    {
        let mut data = table_info.try_borrow_mut_data()?;
        let mut w: &mut [u8] = &mut data[..];
        table.try_serialize(&mut w)?;
    }

    let a = &ctx.accounts;
    let bump = ctx.bumps.app_authority;
    zol_ledger::cpi::delegate_pot(
        CpiContext::new_with_signer(
            zol_ledger::ID,
            zol_ledger::cpi::accounts::DelegatePot {
                payer: a.payer.to_account_info(),
                config: a.config.to_account_info(),
                app: a.app.to_account_info(),
                authority: a.app_authority.to_account_info(),
                receipt: a.receipt.to_account_info(),
                buffer_pot: a.ledger_buffer_pot.to_account_info(),
                delegation_record_pot: a.ledger_delegation_record_pot.to_account_info(),
                delegation_metadata_pot: a.ledger_delegation_metadata_pot.to_account_info(),
                pot: a.pot.to_account_info(),
                owner_program: a.ledger_program.to_account_info(),
                delegation_program: a.delegation_program.to_account_info(),
                system_program: a.system_program.to_account_info(),
            },
            &[&[APP_AUTHORITY_SEED, &[bump]]],
        ),
        expected,
        session_deadline,
        validator,
    )?;
    // The table is delegated with the rent vault as its delegation rent payer, so a
    // stuck table can be rescued by anyone through this program (`rescue_table`).
    top_up_vault(a, ctx.bumps.rent_vault)?;
    // A table that pays its own commits (`crank_close`) is charged MagicBlock's action fee
    // from its lamports; at exactly rent-exempt it would drop below and be refused. The
    // buffer goes back to the creator with the rest of the rent when the table closes.
    anchor_lang::system_program::transfer(
        CpiContext::new(
            anchor_lang::system_program::ID,
            anchor_lang::system_program::Transfer { from: a.payer.to_account_info(), to: a.table.to_account_info() },
        ),
        TABLE_FEE_BUFFER,
    )?;
    delegate_table_via_vault(a, &table.pot_id, validator, ctx.bumps.rent_vault)?;
    emit!(Started { table: table_info.key(), validator, session_deadline });
    Ok(())
}

/// Who may finish and when: the creator or a seated player (anyone after the session
/// deadline), between flips, before the first, or after a flip the VRF never answered.
fn check_finish(t: &Table, who: Pubkey, now: i64) -> Result<()> {
    require!(who == t.creator || t.players.contains(&who) || now >= t.session_deadline, CoinflipError::NotAtTable);
    let stale_flip = t.state == TableState::Flipping && now >= t.flip_requested_at.saturating_add(FLIP_TIMEOUT_SECS);
    require!(matches!(t.state, TableState::Started | TableState::Landed) || stale_flip, CoinflipError::BadTableState);
    Ok(())
}

/// Returns the pot (ledger `close_pot`) and the table to Solana. `payer_seeds` when the
/// payer is one of this program's PDAs (the table, for `crank_close`): a delegated payer
/// needs the validator's magic fee vault. With `pay` (the rent vault and its bump), the
/// pot's return runs `cash_out_table` on Solana (a Magic Action the ledger attaches to the
/// pot, paid from the vault's escrow `["balance", vault, 255]` under the delegation program),
/// so the payout happens exactly when the money is back.
#[allow(clippy::too_many_arguments)]
fn close_in_rollup<'info>(
    payer: &AccountInfo<'info>,
    payer_seeds: Option<&[&[u8]]>,
    table: &mut Account<'info, Table>,
    pot: &UncheckedAccount<'info>,
    app_authority: &UncheckedAccount<'info>,
    app_authority_bump: u8,
    magic_program: &AccountInfo<'info>,
    magic_context: &AccountInfo<'info>,
    fee_vault: Option<&AccountInfo<'info>>,
    pay: Option<(&UncheckedAccount<'info>, u8)>,
) -> Result<()> {
    table.state = TableState::Finished;
    table.exit(&crate::ID)?;
    let auth_bump = [app_authority_bump];
    let auth_seeds: &[&[u8]] = &[APP_AUTHORITY_SEED, &auth_bump];
    let vault_bump = [pay.map(|(_, b)| b).unwrap_or(0)];
    let vault_seeds: [&[u8]; 2] = [RENT_VAULT_SEED, &vault_bump];
    let mut signers: Vec<&[&[u8]]> = vec![auth_seeds];
    signers.extend(payer_seeds);
    // The ledger sees the vault as a signer only if it is marked so: `invoke_signed` with its
    // seeds makes that true.
    let escrow_authority = pay.map(|(vault, _)| {
        let mut info = vault.to_account_info();
        info.is_signer = true;
        info
    });
    if pay.is_some() {
        signers.push(&vault_seeds);
    }
    let action = pay.map(|_| zol_ledger::state::PotAction {
        program: crate::ID,
        accounts: cash_out_metas(table)
            .into_iter()
            .map(|m| zol_ledger::state::ActionAccount { pubkey: Pubkey::new_from_array(m.pubkey.to_bytes()), is_writable: m.is_writable })
            .collect(),
        data: instruction::CashOutTable {}.data(),
        compute_units: 300_000,
    });
    zol_ledger::cpi::close_pot(
        CpiContext::new_with_signer(
            zol_ledger::ID,
            zol_ledger::cpi::accounts::ClosePot {
                payer: payer.clone(),
                authority: app_authority.to_account_info(),
                pot: pot.to_account_info(),
                magic_fee_vault: fee_vault.cloned(),
                escrow_authority,
                magic_program: magic_program.clone(),
                magic_context: magic_context.clone(),
            },
            &signers,
        ),
        action,
    )?;
    let mut bundle = MagicIntentBundleBuilder::new(payer.clone(), magic_context.clone(), magic_program.clone());
    if let Some(v) = fee_vault {
        bundle = bundle.magic_fee_vault(v.clone());
    }
    let bundle = bundle.commit_and_undelegate(&[table.to_account_info()]);
    match payer_seeds {
        Some(s) => bundle.build_and_invoke_signed(&[s])?,
        None => bundle.build_and_invoke()?,
    }
    emit!(Finished { table: table.key(), winner: table.winner });
    Ok(())
}

/// MagicBlock's fee vault for a validator (`["magic-fee-vault", validator]` under the
/// delegation program): a delegated payer's commit fees go there.
pub fn magic_fee_vault(validator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"magic-fee-vault", validator.as_ref()], &ephemeral_rollups_sdk::id()).0
}

/// The accounts of `cash_out_table` for this table, in its order.
fn cash_out_metas(t: &Table) -> Vec<ShortAccountMeta> {
    use zol_ledger::state::seeds;
    let l = &zol_ledger::ID;
    let pda = |s: &[&[u8]]| Pubkey::find_program_address(s, l).0;
    let app = pda(&[seeds::APP, crate::ID.as_ref()]);
    let meta = |pubkey: Pubkey, is_writable: bool| ShortAccountMeta { pubkey: pubkey.to_bytes().into(), is_writable };
    vec![
        meta(pda(&[seeds::CONFIG]), false),
        meta(app, false),
        meta(pda(&[seeds::RECEIPT, t.pot.as_ref()]), true),
        meta(t.pot, false),
        meta(table_address(&t.pot_id), true),
        meta(t.creator, true),
        meta(pda(&[seeds::BALANCE, app.as_ref(), t.players[0].as_ref()]), true),
        meta(pda(&[seeds::BALANCE, app.as_ref(), t.players[1].as_ref()]), true),
        meta(zol_ledger::ID, false),
    ]
}

/// Tops the rent vault up to `RENT_VAULT_TARGET` from `start`'s payer.
fn top_up_vault<'info>(a: &Start<'info>, _vault_bump: u8) -> Result<()> {
    let have = a.rent_vault.lamports();
    if have < RENT_VAULT_TARGET {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                anchor_lang::system_program::ID,
                anchor_lang::system_program::Transfer { from: a.payer.to_account_info(), to: a.rent_vault.to_account_info() },
            ),
            RENT_VAULT_TARGET - have,
        )?;
    }
    Ok(())
}

/// The SDK's `delegate_account`, with the rent vault as payer: the vault funds the
/// buffer and the delegation deposit, so it is the delegation's `rent_payer`, and this
/// program can sign for it in `rescue_table`.
fn delegate_table_via_vault<'info>(a: &Start<'info>, pot_id: &[u8; 32], validator: Pubkey, vault_bump: u8) -> Result<()> {
    let table = a.table.to_account_info();
    let vault = a.rent_vault.to_account_info();
    let system = a.system_program.to_account_info();
    let buffer = a.buffer_table.to_account_info();
    let (_, table_bump) = Pubkey::find_program_address(&[TABLE_SEED, pot_id], &crate::ID);
    let (_, buffer_bump) = Pubkey::find_program_address(&[ephemeral_rollups_sdk::pda::DELEGATE_BUFFER_TAG, table.key.as_ref()], &crate::ID);
    let table_seeds: &[&[u8]] = &[TABLE_SEED, pot_id, &[table_bump]];
    let buffer_seeds: &[&[u8]] = &[ephemeral_rollups_sdk::pda::DELEGATE_BUFFER_TAG, table.key.as_ref(), &[buffer_bump]];
    let vault_seeds: &[&[u8]] = &[RENT_VAULT_SEED, &[vault_bump]];

    let len = table.data_len();
    create_pda(&buffer, &crate::ID, len, &[buffer_seeds, vault_seeds], &system, &vault, true)?;
    buffer.try_borrow_mut_data()?.copy_from_slice(&table.try_borrow_data()?);
    table.try_borrow_mut_data()?.fill(0);
    table.assign(&anchor_lang::system_program::ID);
    anchor_lang::system_program::assign(
        CpiContext::new_with_signer(
            anchor_lang::system_program::ID,
            anchor_lang::system_program::Assign { account_to_assign: table.clone() },
            &[table_seeds],
        ),
        &ephemeral_rollups_sdk::id(),
    )?;
    cpi_delegate(
        &vault,
        &table,
        &a.owner_program.to_account_info(),
        &buffer,
        &a.delegation_record_table.to_account_info(),
        &a.delegation_metadata_table.to_account_info(),
        &system,
        &[table_seeds, vault_seeds],
        DelegateAccountArgs { commit_frequency_ms: u32::MAX, seeds: vec![TABLE_SEED.to_vec(), pot_id.to_vec()], validator: Some(validator) },
    )?;
    close_pda_with_system_transfer(&buffer, &[buffer_seeds], &vault, &system)?;
    Ok(())
}

/// Seat 0 or 1 from the oracle's randomness.
pub fn outcome(randomness: &[u8; 32]) -> u8 {
    ephemeral_rollups_sdk::vrf::rnd::random_u8(randomness) & 1
}

pub fn app_authority_address() -> Pubkey {
    Pubkey::find_program_address(&[APP_AUTHORITY_SEED], &crate::ID).0
}

pub fn table_address(pot_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[TABLE_SEED, pot_id], &crate::ID).0
}

fn load_receipt(info: &UncheckedAccount, table: &Table) -> Result<PotReceipt> {
    require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
    let data = info.try_borrow_data()?;
    let r = PotReceipt::try_deserialize(&mut &data[..])?;
    require_keys_eq!(r.pot, table.pot, CoinflipError::BadAccount);
    Ok(r)
}

// ---------------------------------------------------------------- state

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum TableState {
    Open,
    Started,
    Flipping,
    Landed,
    Finished,
}

/// One table per pot. Holds no player keys: seats are the ledger receipt's.
#[account]
#[derive(InitSpace)]
pub struct Table {
    pub pot: Pubkey,
    pub pot_id: [u8; 32],
    pub stake: u64,
    /// Rollup validator both the pot and the table are delegated to.
    pub validator: Pubkey,
    /// Paid the table's rent in `create_table`; `close_table` returns it here.
    pub creator: Pubkey,
    pub state: TableState,
    /// Winning seat index after `land`, `NO_WINNER` before.
    pub winner: u8,
    pub randomness: [u8; 32],
    pub bump: u8,
    /// Flips landed so far (a table plays as many as its seats' stakes allow).
    pub flips: u32,
    /// When the last flip asked the oracle, or `start` ran (unix seconds): the table's last
    /// activity, from which `crank_close` counts idleness.
    pub flip_requested_at: i64,
    /// The seated keys, copied from the receipt by `start` (default before).
    pub players: [Pubkey; 2],
    /// The ledger session deadline `start` asked for (unix seconds).
    pub session_deadline: i64,
    /// Winning seats, newest in bit 0 (the last 64 flips).
    pub results: u64,
}

// ---------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct Register<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: this program; the ledger checks it is executable
    #[account(address = crate::ID)]
    pub this_program: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: ledger Config; checked by the ledger
    pub config: UncheckedAccount<'info>,
    /// CHECK: ledger App PDA; created by the ledger
    #[account(mut)]
    pub app: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(pot_id: [u8; 32])]
pub struct CreateTable<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(init, payer = payer, space = 8 + Table::INIT_SPACE, seeds = [TABLE_SEED, pot_id.as_ref()], bump)]
    pub table: Box<Account<'info, Table>>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    pub config: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    #[account(mut)]
    pub app: UncheckedAccount<'info>,
    /// CHECK: created by the ledger at its pot address
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: created by the ledger
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Join<'info> {
    pub session_key: Signer<'info>,
    #[account(seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Box<Account<'info, Table>>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    pub config: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    #[account(mut)]
    pub app: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger (seeds from the session key)
    #[account(mut)]
    pub balance: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger and `has_one`
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: owner and pot checked here, seeds by the ledger
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
}

#[delegate]
#[derive(Accounts)]
pub struct Start<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the table PDA; untyped because it changes owner in this instruction
    #[account(mut, del)]
    pub table: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    pub config: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    #[account(mut)]
    pub app: UncheckedAccount<'info>,
    /// CHECK: owner and pot checked here, seeds by the ledger
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: checked against the table and by the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: ledger's delegation buffer for the pot (["buffer", pot] under the ledger)
    #[account(mut)]
    pub ledger_buffer_pot: UncheckedAccount<'info>,
    /// CHECK: delegation program record for the pot
    #[account(mut)]
    pub ledger_delegation_record_pot: UncheckedAccount<'info>,
    /// CHECK: delegation program metadata for the pot
    #[account(mut)]
    pub ledger_delegation_metadata_pot: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    /// CHECK: system-owned PDA holding lamports only; pays the table's delegation deposit
    #[account(mut, seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
}

#[vrf]
#[derive(Accounts)]
pub struct Flip<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: the table's pot (`has_one`); owner checked in the handler
    pub pot: UncheckedAccount<'info>,
    /// CHECK: VRF oracle queue; checked by the VRF program
    #[account(mut)]
    pub oracle_queue: UncheckedAccount<'info>,
}

#[vrf_callback]
#[derive(Accounts)]
pub struct Land<'info> {
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: owner checked here, everything else by the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct Finish<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: checked by `has_one` and the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct FinishAndPay<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: checked by `has_one` and the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    /// CHECK: escrow authority of the Magic Action (its escrow pays the Solana fee)
    #[account(seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
}

/// No signer: a scheduled task runs it.
#[commit]
#[derive(Accounts)]
pub struct CrankClose<'info> {
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: checked by `has_one` and the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    /// CHECK: escrow authority of the Magic Action
    #[account(seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
    /// CHECK: the table validator's magic fee vault (checked in the handler); the table pays as
    /// a delegated account, and those pay commit fees there
    #[account(mut)]
    pub magic_fee_vault: UncheckedAccount<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct ScheduleCloser<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: checked by `has_one`
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    /// CHECK: escrow authority of the Magic Action
    #[account(seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
    /// CHECK: must be `magic_fee_vault(table.validator)` (checked in the handler); only named
    /// in the scheduled instruction
    pub magic_fee_vault: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CashOutTable<'info> {
    /// CHECK: ledger Config; checked by the ledger
    pub config: UncheckedAccount<'info>,
    /// CHECK: ledger App; checked by the ledger
    pub app: UncheckedAccount<'info>,
    /// CHECK: owner and address checked in the handler, the rest by the ledger
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger (address from the receipt) and its owner here
    pub pot: UncheckedAccount<'info>,
    /// CHECK: address from the receipt's pot id; closed here once the receipt is terminal
    #[account(mut)]
    pub table: UncheckedAccount<'info>,
    /// CHECK: must be the table's creator
    #[account(mut)]
    pub creator: UncheckedAccount<'info>,
    /// CHECK: the two seats' balances in receipt order; checked by the ledger
    #[account(mut)]
    pub balance0: UncheckedAccount<'info>,
    /// CHECK: see `balance0`
    #[account(mut)]
    pub balance1: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct RescueTable<'info> {
    /// CHECK: the stuck table, owned by the delegation program; checked in the handler
    #[account(mut)]
    pub table: UncheckedAccount<'info>,
    /// CHECK: the ledger receipt of the table's pot; owner, address and state checked
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: the table's delegation rent payer
    #[account(mut, seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
    /// CHECK: delegation program PDA ["undelegation-request", table]; checked by it
    #[account(mut)]
    pub undelegation_request: UncheckedAccount<'info>,
    /// CHECK: delegation program PDA ["delegation", table]
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,
    /// CHECK: delegation program PDA ["delegation-metadata", table]
    #[account(mut)]
    pub delegation_metadata: UncheckedAccount<'info>,
    /// CHECK: delegation program PDA ["state-diff", table] (pending commit, if any)
    #[account(mut)]
    pub commit_state: UncheckedAccount<'info>,
    /// CHECK: delegation program PDA ["commit-state-record", table]
    #[account(mut)]
    pub commit_record: UncheckedAccount<'info>,
    /// CHECK: the table's validator; refunded a pending commit's rent, if any
    #[account(mut)]
    pub validator: UncheckedAccount<'info>,
    /// CHECK: this program (the table's owner program)
    #[account(address = crate::ID)]
    pub this_program: UncheckedAccount<'info>,
    /// CHECK: the delegation program
    #[account(address = ephemeral_rollups_sdk::id())]
    pub delegation_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseTable<'info> {
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = creator @ CoinflipError::BadAccount, close = creator)]
    pub table: Account<'info, Table>,
    /// CHECK: owner and address checked in the handler
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: receives the rent; must be the table's creator
    #[account(mut)]
    pub creator: UncheckedAccount<'info>,
}

// ---------------------------------------------------------------- events and errors

#[event]
pub struct TableCreated {
    pub table: Pubkey,
    pub pot: Pubkey,
    pub stake: u64,
    pub validator: Pubkey,
}
#[event]
pub struct Joined {
    pub table: Pubkey,
    pub player: Pubkey,
    pub seat: u8,
}
#[event]
pub struct Started {
    pub table: Pubkey,
    pub validator: Pubkey,
    pub session_deadline: i64,
}
#[event]
pub struct FlipRequested {
    pub table: Pubkey,
}
#[event]
pub struct Landed {
    pub table: Pubkey,
    pub winner: u8,
    pub amount: u64,
}
#[event]
pub struct RescueRequested {
    pub table: Pubkey,
}
#[event]
pub struct TableRescued {
    pub table: Pubkey,
}
#[event]
pub struct TableClosed {
    pub table: Pubkey,
    pub creator: Pubkey,
    pub pot_state: u8,
}
#[event]
pub struct CloserScheduled {
    pub table: Pubkey,
    pub iterations: i64,
}
#[event]
pub struct Finished {
    pub table: Pubkey,
    pub winner: u8,
}

#[error_code]
pub enum CoinflipError {
    #[msg("stake must be positive")]
    ZeroStake,
    #[msg("arithmetic overflow")]
    Overflow,
    #[msg("wrong table state for this instruction")]
    BadTableState,
    #[msg("account does not belong to this table")]
    BadAccount,
    #[msg("player already holds a seat")]
    AlreadySeated,
    #[msg("both seats are taken")]
    TableFull,
    #[msg("the table needs two players")]
    NotFull,
    #[msg("stake is not a deposit denomination's net value, or a seat does not hold the table stake")]
    BadStake,
    #[msg("oracle queue is not one of MagicBlock's")]
    BadQueue,
    #[msg("a rollup validator must be named")]
    BadValidator,
    #[msg("the pot is still live: not cashed out or refunded")]
    PotLive,
    #[msg("the table is not delegated")]
    NotDelegated,
    #[msg("rescue needs the pot refunded by the ledger")]
    NotRefunded,
    #[msg("a seat cannot cover another stake")]
    SeatShort,
    #[msg("rounds must be 1 to MAX_ROUNDS")]
    BadRounds,
    #[msg("only a seated player (or, to finish, the table's creator) may do that")]
    NotAtTable,
}
