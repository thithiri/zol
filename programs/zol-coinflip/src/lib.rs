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
//! | `create_updown_table` | Solana | `create_table` for ZEC Up/Down: bets on the price instead of coin flips |
//! | `bet`          | Rollup  | Up/Down: a seated player bets Up or Down at the oracle's price, for `UPDOWN_ROUND_SECS` |
//! | `settle_bet`   | Rollup  | Anyone (and `crank_close`), within `SETTLE_WINDOW_SECS` of expiry: pays 1.9× on a right call, refunds a tie |
//! | `finish`       | Rollup  | The creator or a seated player (anyone after the session deadline): CPI `close_pot`, commits and undelegates the table |
//! | `finish_and_pay` | Rollup | `finish`, and the table's return schedules `cash_out_table` on Solana (a Magic Action) |
//! | `crank_close`  | Rollup  | No signer (a scheduled task): finishes and pays a table that is idle, short or near its deadline |
//! | `schedule_closer` | Rollup | Anyone: schedules `crank_close` for a table until its session deadline |
//! | `cash_out_table` | Solana | Anyone (and the Magic Action): ledger `cash_out` for both seats, then closes the table |
//! | `close_table`  | Solana  | Anyone, once the ledger's receipt is terminal: closes the table, rent to its creator |
//!
//! Sealed-bid auctions (commit, then reveal; first price) on a ledger pot, the lot a certificate:
//!
//! | Instruction    | Runs on | What it does |
//! |----------------|---------|--------------|
//! | `create_auction` | Solana | The seller opens a pot (seat 0, a 1-zat stake) and an `Auction` with its deadlines and deposit |
//! | `register_bidder` | Solana | A bidder pays the deposit (the bid cap) into the pot, before the register deadline |
//! | `start_auction` | Solana | Anyone, at the register deadline: pot and auction into the rollup |
//! | `commit_bid`   | Rollup  | A bidder posts `sha256("ZOL/bid/v1" ‖ auction ‖ bidder ‖ amount ‖ salt)` before the commit deadline |
//! | `reveal_bid`   | Rollup  | Between the commit and reveal deadlines: the amount and salt, checked against the commitment |
//! | `end_auction`  | Rollup  | No signer, after the reveal deadline: highest revealed bid pays its bid to the seller; unrevealed commitments lose 10% of the deposit to those who revealed; the pot returns and `cash_out_auction` pays every seat |
//! | `schedule_auction_end` | Rollup | Anyone: the rollup runs `end_auction` by itself |
//! | `cash_out_auction` | Solana | Anyone (and the pot's return action): ledger `cash_out` for every seat |
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
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::utils::{close_pda_with_system_transfer, create_pda};
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;
#[allow(deprecated)]
use ephemeral_rollups_sdk::crank::{ScheduleCrankCpi, ScheduleTaskArgs};
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder, MagicIntentBundleBuilder};
use ephemeral_rollups_sdk::ShortAccountMeta;
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
/// Table games: coin flips (VRF) or ZEC Up/Down (price oracle).
pub const GAME_COINFLIP: u8 = 0;
pub const GAME_UPDOWN: u8 = 1;
/// Up/Down: a right call is paid 1.9× its stake (0.9 stake from the other seat); a tie refunds.
pub const UPDOWN_PAYOUT_BPS: u64 = 19_000;
pub const UPDOWN_ROUND_SECS: i64 = 30;
/// A bet settles at a price published at or after its expiry, within this window; after it
/// the bet is void (nothing moves), so waiting for a better price never pays.
pub const SETTLE_WINDOW_SECS: i64 = 10;
/// The oldest price `bet` and `settle_bet` accept.
pub const MAX_PRICE_AGE_SECS: i64 = 5;
/// MagicBlock's price oracle program and its ZEC/USD feed (Pyth Lazer id 66): a Pyth
/// `PriceUpdateV2` body with full verification; live in the rollup only.
pub const PRICE_PROGRAM: Pubkey = pubkey!("PriCems5tHihc6UDXDjzjeawomAwBduWMGAi8ZUjppd");
pub const ZEC_USD_FEED: Pubkey = pubkey!("6XWQr2Y1XEpJrCdVbYGupnDeb3wkR4YXazVXdgn2Lwpg");
pub const ZEC_USD_FEED_ID: [u8; 32] = [
    0x52, 0x1b, 0x1d, 0x3a, 0x3a, 0x32, 0xdb, 0xf1, 0xf9, 0x1f, 0xc6, 0x02, 0xf6, 0x3f, 0xde, 0x2b,
    0x8e, 0x43, 0x53, 0xa7, 0x94, 0xb1, 0xa0, 0x34, 0x84, 0xb7, 0x19, 0x19, 0xe7, 0x68, 0x2b, 0x4d,
];

/// Auctions: the seller plus at most this many bidders (the pot's return action names every
/// seat's balance, within the ledger's 16-account action limit).
pub const AUCTION_SEATS: usize = 9;
pub const AUCTION_SEED: &[u8] = b"auction";
/// The seller's own stake (seat 0), so the pot has a seat to pay the price into.
pub const SELLER_STAKE: u64 = 1;
/// After the register deadline, the pot stays open this long for `start_auction`.
pub const AUCTION_START_GRACE_SECS: i64 = 120;
/// The ledger session deadline is this long after the reveal deadline.
pub const AUCTION_SESSION_MARGIN_SECS: i64 = 900;
/// The scheduled `end_auction` keeps running this long past the reveal deadline (a run after
/// the end fails harmlessly, the auction having left the rollup).
pub const AUCTION_END_TAIL_SECS: i64 = 60;
/// A commitment revealed too late, or never: 1/10 of the deposit, to those who revealed.
pub const NO_REVEAL_PENALTY_DIV: u64 = 10;
/// Each phase lasts at most this long.
pub const MAX_PHASE_SECS: i64 = 1_800;

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
        create_table_for(ctx, pot_id, stake, open_secs, validator, GAME_COINFLIP)
    }

    /// `create_table` for ZEC Up/Down: the seats bet on the price (`bet`, `settle_bet`).
    pub fn create_updown_table(ctx: Context<CreateTable>, pot_id: [u8; 32], stake: u64, open_secs: i64, validator: Pubkey) -> Result<()> {
        create_table_for(ctx, pot_id, stake, open_secs, validator, GAME_UPDOWN)
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
        require!(ctx.accounts.table.game == GAME_COINFLIP, CoinflipError::WrongGame);
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
        if winner == 1 {
            t.wins = t.wins.saturating_add(1);
        }
        emit!(Landed { table: t.key(), winner, amount });
        Ok(())
    }

    /// Rollup. The table's creator or a seated player, between flips (or before the first):
    /// returns the pot (ledger `close_pot`) and the table to Solana. Anyone may once the
    /// session deadline has passed, so the pot comes home with its result before the ledger
    /// would refund the buy-ins. A flip whose VRF answer has not landed after
    /// `FLIP_TIMEOUT_SECS` may be abandoned; its stakes stay where they were.
    /// Rollup, Up/Down. The seated player bets the price goes Up (or Down) over the next
    /// `UPDOWN_ROUND_SECS`, one stake, at the oracle's current price. The other seat must
    /// cover the 0.9-stake win, and there is one open bet at a time.
    pub fn bet(ctx: Context<Bet>, up: bool) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let who = ctx.accounts.payer.key();
        let t = &ctx.accounts.table;
        require!(t.game == GAME_UPDOWN, CoinflipError::WrongGame);
        require!(matches!(t.state, TableState::Started | TableState::Landed), CoinflipError::BadTableState);
        let seat = t.players.iter().position(|p| *p == who).ok_or(CoinflipError::NotAtTable)? as u8;
        require!(!t.bet.open, CoinflipError::BetOpen);
        let amounts = pot_amounts(&ctx.accounts.pot)?;
        let win = updown_win(t.stake);
        require!(amounts[seat as usize] >= t.stake && amounts[1 - seat as usize] >= win, CoinflipError::SeatShort);
        let (price, _) = read_price(&ctx.accounts.oracle, now)?;
        let t = &mut ctx.accounts.table;
        t.bet = BetSlot { open: true, up, seat, open_price: price, expiry: now + UPDOWN_ROUND_SECS, settle_price: 0 };
        t.flip_requested_at = now; // activity, for `crank_close`
        emit!(BetPlaced { table: t.key(), up, open_price: price, expiry: t.bet.expiry });
        Ok(())
    }

    /// Rollup, anyone (the page at expiry, `crank_close` every few seconds). Settles the open
    /// bet with a price published at or after its expiry: a tie moves nothing, a right call
    /// is paid 0.9 stake by the other seat, a wrong one pays its stake to it. Past
    /// `SETTLE_WINDOW_SECS` the bet is void instead.
    pub fn settle_bet(ctx: Context<SettleBet>) -> Result<()> {
        let bump = ctx.bumps.app_authority;
        let a = ctx.accounts;
        settle_open_bet(&mut a.table, &a.pot, &a.oracle, &a.app_authority, bump, Clock::get()?.unix_timestamp, true)
    }

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
    pub fn crank_close(mut ctx: Context<CrankClose>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        if ctx.accounts.table.bet.open {
            // An Up/Down bet first: settle it once its price is in, or void it past the window;
            // the table may close on a later run.
            if now >= ctx.accounts.table.bet.expiry {
                let bump = ctx.bumps.app_authority;
                let a = &mut ctx.accounts;
                settle_open_bet(&mut a.table, &a.pot, &a.oracle, &a.app_authority, bump, now, false)?;
            }
            return Ok(());
        }
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
                oracle: ZEC_USD_FEED,
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
                a.oracle.to_account_info(),
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

    // ------------------------------------------------------------ sealed-bid auctions

    /// Solana. The seller (a session key with a ledger balance) opens an auction: a pot with
    /// the seller in seat 0 (`SELLER_STAKE`), and the `Auction` with absolute deadlines for the
    /// register, commit and reveal phases. The lot is a certificate: this account, kept on
    /// Solana with the winner and price once the auction ends.
    #[allow(clippy::too_many_arguments)]
    pub fn create_auction(
        ctx: Context<CreateAuction>,
        pot_id: [u8; 32],
        deposit: u64,
        register_secs: i64,
        commit_secs: i64,
        reveal_secs: i64,
        validator: Pubkey,
    ) -> Result<()> {
        require!(deposit >= NO_REVEAL_PENALTY_DIV, CoinflipError::BadAuction);
        for secs in [register_secs, commit_secs, reveal_secs] {
            require!(secs > 0 && secs <= MAX_PHASE_SECS, CoinflipError::BadAuction);
        }
        require_keys_neq!(validator, Pubkey::default(), CoinflipError::BadValidator);
        let now = Clock::get()?.unix_timestamp;
        let register_deadline = now + register_secs;
        let a = &ctx.accounts;
        let bump = ctx.bumps.app_authority;
        let signer: &[&[&[u8]]] = &[&[APP_AUTHORITY_SEED, &[bump]]];
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
                signer,
            ),
            pot_id,
            register_deadline + AUCTION_START_GRACE_SECS,
        )?;
        zol_ledger::cpi::buy_in(
            CpiContext::new_with_signer(
                zol_ledger::ID,
                zol_ledger::cpi::accounts::BuyIn {
                    config: a.config.to_account_info(),
                    app: a.app.to_account_info(),
                    authority: a.app_authority.to_account_info(),
                    session_key: a.seller.to_account_info(),
                    balance: a.seller_balance.to_account_info(),
                    pot: a.pot.to_account_info(),
                    receipt: a.receipt.to_account_info(),
                },
                signer,
            ),
            SELLER_STAKE,
        )?;
        let x = &mut ctx.accounts.auction;
        x.pot = ctx.accounts.pot.key();
        x.pot_id = pot_id;
        x.creator = ctx.accounts.payer.key();
        x.validator = validator;
        x.deposit = deposit;
        x.register_deadline = register_deadline;
        x.commit_deadline = register_deadline + commit_secs;
        x.reveal_deadline = register_deadline + commit_secs + reveal_secs;
        x.state = AuctionState::Registering;
        x.seat_count = 1;
        x.players = [Pubkey::default(); AUCTION_SEATS];
        x.players[0] = ctx.accounts.seller.key();
        x.commits = [[0; 32]; AUCTION_SEATS];
        x.bids = [0; AUCTION_SEATS];
        x.committed = 0;
        x.revealed = 0;
        x.winner = NO_WINNER;
        x.price = 0;
        x.bump = ctx.bumps.auction;
        emit!(AuctionCreated { auction: x.key(), deposit, register_deadline, commit_deadline: x.commit_deadline, reveal_deadline: x.reveal_deadline });
        Ok(())
    }

    /// Solana. A bidder's session key pays the deposit (the bid cap) into the pot, before the
    /// register deadline. Every bidder pays the same, so deposits say nothing about bids.
    pub fn register_bidder(ctx: Context<RegisterBidder>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let x = &ctx.accounts.auction;
        require!(x.state == AuctionState::Registering && now < x.register_deadline, CoinflipError::AuctionPhase);
        require!((x.seat_count as usize) < AUCTION_SEATS, CoinflipError::TableFull);
        let me = ctx.accounts.session_key.key();
        require!(!x.players[..x.seat_count as usize].contains(&me), CoinflipError::AlreadySeated);
        let a = &ctx.accounts;
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
            a.auction.deposit,
        )?;
        let x = &mut ctx.accounts.auction;
        let seat = x.seat_count as usize;
        x.players[seat] = me;
        x.seat_count += 1;
        emit!(Registered { auction: x.key(), bidder: me, seat: seat as u8 });
        Ok(())
    }

    /// Solana, anyone, from the register deadline (or once full) until the pot's open deadline:
    /// the pot and the auction move into the rollup. Needs at least one bidder; an auction
    /// nobody joined is refunded by the ledger after its open deadline.
    pub fn start_auction(ctx: Context<StartAuction>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let info = ctx.accounts.auction.to_account_info();
        require_keys_eq!(*info.owner, crate::ID, CoinflipError::BadAccount);
        let mut x = {
            let data = info.try_borrow_data()?;
            Box::new(Auction::try_deserialize(&mut &data[..])?)
        };
        let (expected, _) = Pubkey::find_program_address(&[AUCTION_SEED, &x.pot_id], &crate::ID);
        require_keys_eq!(expected, info.key(), CoinflipError::BadAccount);
        require_keys_eq!(x.pot, ctx.accounts.pot.key(), CoinflipError::BadAccount);
        require!(x.state == AuctionState::Registering, CoinflipError::AuctionPhase);
        require!(now >= x.register_deadline || x.seat_count as usize == AUCTION_SEATS, CoinflipError::AuctionPhase);
        require!(x.seat_count >= 2, CoinflipError::NotFull);
        let r = {
            let rinfo = &ctx.accounts.receipt;
            require_keys_eq!(*rinfo.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let data = rinfo.try_borrow_data()?;
            Box::new(PotReceipt::try_deserialize(&mut &data[..])?)
        };
        require_keys_eq!(r.pot, x.pot, CoinflipError::BadAccount);
        let n = r.seat_count as usize;
        require!(n == x.seat_count as usize && r.players[..n] == x.players[..n], CoinflipError::BadAccount);
        let expected: Vec<SeatStake> = (0..n).map(|i| SeatStake { player: r.players[i], amount: r.buy_ins[i] }).collect();
        x.state = AuctionState::Bidding;
        {
            let mut data = info.try_borrow_mut_data()?;
            let mut w: &mut [u8] = &mut data[..];
            x.try_serialize(&mut w)?;
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
            x.reveal_deadline + AUCTION_SESSION_MARGIN_SECS,
            x.validator,
        )?;
        // The auction pays its own commits at the end (a scheduled task has no signer): a buffer.
        anchor_lang::system_program::transfer(
            CpiContext::new(
                anchor_lang::system_program::ID,
                anchor_lang::system_program::Transfer { from: a.payer.to_account_info(), to: info.clone() },
            ),
            TABLE_FEE_BUFFER,
        )?;
        let pot_id = x.pot_id;
        ctx.accounts.delegate_auction(
            &ctx.accounts.payer,
            &[AUCTION_SEED, &pot_id],
            DelegateConfig { validator: Some(x.validator), ..Default::default() },
        )?;
        emit!(AuctionStarted { auction: info.key(), bidders: x.seat_count - 1 });
        Ok(())
    }

    /// Rollup. A registered bidder posts (or replaces) their sealed bid before the commit
    /// deadline: `bid_commitment(auction, bidder, amount, salt)`.
    pub fn commit_bid(ctx: Context<AuctionBid>, commitment: [u8; 32]) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let x = &mut ctx.accounts.auction;
        require!(x.state == AuctionState::Bidding && now < x.commit_deadline, CoinflipError::AuctionPhase);
        let seat = bidder_seat(x, &ctx.accounts.bidder.key())?;
        x.commits[seat] = commitment;
        x.committed |= 1 << seat;
        Ok(())
    }

    /// Rollup. Between the commit and reveal deadlines the bidder reveals: the amount (at most
    /// the deposit) and salt must match the commitment.
    pub fn reveal_bid(ctx: Context<AuctionBid>, amount: u64, salt: [u8; 32]) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let key = ctx.accounts.auction.key();
        let x = &mut ctx.accounts.auction;
        require!(x.state == AuctionState::Bidding && now >= x.commit_deadline && now < x.reveal_deadline, CoinflipError::AuctionPhase);
        let bidder = ctx.accounts.bidder.key();
        let seat = bidder_seat(x, &bidder)?;
        require!(x.committed & (1 << seat) != 0, CoinflipError::NoCommitment);
        require!(amount > 0 && amount <= x.deposit, CoinflipError::BadBid);
        require!(bid_commitment(&key, &bidder, amount, &salt) == x.commits[seat], CoinflipError::BadReveal);
        x.bids[seat] = amount;
        x.revealed |= 1 << seat;
        Ok(())
    }

    /// Rollup, no signer (the scheduled task, or anyone) after the reveal deadline. The highest
    /// revealed bid wins (ties: the earlier registration) and pays its bid to the seller.
    /// Each commitment never revealed pays 1/10 of the deposit, shared by those who revealed.
    /// Then the pot returns to Solana with `cash_out_auction` attached, and the auction too,
    /// where it stays as the certificate. Before the deadline it does nothing.
    pub fn end_auction(mut ctx: Context<EndAuction>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let x = &ctx.accounts.auction;
        if x.state != AuctionState::Bidding || now < x.reveal_deadline {
            return Ok(());
        }
        let n = x.seat_count as usize;
        let revealers: Vec<usize> = (1..n).filter(|s| x.revealed & (1 << s) != 0).collect();
        let quitters: Vec<usize> = (1..n).filter(|s| x.committed & (1 << s) != 0 && x.revealed & (1 << s) == 0).collect();
        let winner = revealers.iter().copied().fold(None::<usize>, |best, s| match best {
            Some(b) if x.bids[b] >= x.bids[s] => Some(b),
            _ => Some(s),
        });
        let mut moves: Vec<(u8, u8, u64)> = vec![];
        if let Some(w) = winner {
            moves.push((w as u8, 0, x.bids[w]));
        }
        if !revealers.is_empty() {
            let penalty = x.deposit / NO_REVEAL_PENALTY_DIV;
            for q in &quitters {
                let share = penalty / revealers.len() as u64;
                for (i, r) in revealers.iter().enumerate() {
                    let extra = if i == 0 { penalty - share * revealers.len() as u64 } else { 0 };
                    if share + extra > 0 {
                        moves.push((*q as u8, *r as u8, share + extra));
                    }
                }
            }
        }
        let bump = ctx.bumps.app_authority;
        for (from, to, amount) in moves {
            zol_ledger::cpi::move_seat(
                CpiContext::new_with_signer(
                    zol_ledger::ID,
                    zol_ledger::cpi::accounts::MoveSeat {
                        authority: ctx.accounts.app_authority.to_account_info(),
                        pot: ctx.accounts.pot.to_account_info(),
                        app_authority: ctx.accounts.app_authority.to_account_info(),
                    },
                    &[&[APP_AUTHORITY_SEED, &[bump]]],
                ),
                from,
                to,
                amount,
            )?;
        }
        require_keys_eq!(ctx.accounts.magic_fee_vault.key(), magic_fee_vault(&ctx.accounts.auction.validator), CoinflipError::BadAccount);
        let a = &mut ctx.accounts;
        a.auction.state = AuctionState::Ended;
        a.auction.winner = winner.map(|w| w as u8).unwrap_or(NO_WINNER);
        a.auction.price = winner.map(|w| a.auction.bids[w]).unwrap_or(0);
        emit!(AuctionEnded { auction: a.auction.key(), winner: winner.map(|w| a.auction.players[w]), price: a.auction.price, revealed: revealers.len() as u8, forfeited: quitters.len() as u8 });
        a.auction.exit(&crate::ID)?;
        close_auction_in_rollup(a, bump, ctx.bumps.rent_vault)
    }

    /// Rollup, anyone: the rollup runs `end_auction` every few seconds until well past the
    /// reveal deadline, so the auction ends and pays even if every client is gone.
    #[allow(deprecated)]
    pub fn schedule_auction_end<'info>(ctx: Context<'info, ScheduleAuctionEnd<'info>>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let x = &ctx.accounts.auction;
        require!(x.state == AuctionState::Bidding, CoinflipError::AuctionPhase);
        let left_ms = (x.reveal_deadline + AUCTION_END_TAIL_SECS - now).max(0).saturating_mul(1000);
        let iterations = left_ms / CLOSER_INTERVAL_MS + 1;
        let a = &ctx.accounts;
        let ix = Instruction {
            program_id: crate::ID,
            accounts: accounts::EndAuction {
                auction: a.auction.key(),
                pot: a.pot.key(),
                app_authority: a.app_authority.key(),
                ledger_program: a.ledger_program.key(),
                rent_vault: a.rent_vault.key(),
                magic_fee_vault: magic_fee_vault(&x.validator),
                magic_program: a.magic_program.key(),
                magic_context: a.magic_context.key(),
            }
            .to_account_metas(None),
            data: instruction::EndAuction {}.data(),
        };
        let task_id = i64::from_le_bytes(x.pot_id[8..16].try_into().unwrap()) & i64::MAX;
        ScheduleCrankCpi {
            payer: &ctx.accounts.payer,
            magic_program: &ctx.accounts.magic_program,
            instruction_accounts: &[
                a.auction.to_account_info(),
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
        Ok(())
    }

    /// Solana, anyone; also the action `end_auction` attaches to the pot's return. Once the
    /// pot is back, ledger `cash_out` for every seat (balances in seat order, then anything
    /// else, which is ignored). A no-op until then.
    pub fn cash_out_auction<'info>(ctx: Context<'info, CashOutAuction<'info>>) -> Result<()> {
        let a = &ctx.accounts;
        let r = {
            let info = &a.receipt;
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            Box::new(PotReceipt::try_deserialize(&mut &data[..])?)
        };
        if r.state != PotState::Delegated || *a.pot.owner != zol_ledger::ID {
            msg!("pot not back on Solana, or already paid");
            return Ok(());
        }
        let n = r.seat_count as usize;
        require!(ctx.remaining_accounts.len() >= n, CoinflipError::BadAccount);
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
            .with_remaining_accounts(ctx.remaining_accounts[..n].to_vec()),
        )
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

fn create_table_for(ctx: Context<CreateTable>, pot_id: [u8; 32], stake: u64, open_secs: i64, validator: Pubkey, game: u8) -> Result<()> {
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
    t.game = game;
    t.bet = BetSlot::default();
    t.wins = 0;
    t.ties = 0;
    emit!(TableCreated { table: t.key(), pot: t.pot, stake, validator });
    Ok(())
}

/// The other seat's payment on a right Up/Down call: 0.9 stake at 1.9×.
pub fn updown_win(stake: u64) -> u64 {
    stake.saturating_mul(UPDOWN_PAYOUT_BPS - 10_000) / 10_000
}

fn pot_amounts(pot: &UncheckedAccount) -> Result<[u64; zol_ledger::state::limits::MAX_SEATS]> {
    let info = pot.to_account_info();
    require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
    let data = info.try_borrow_data()?;
    Ok(Pot::try_deserialize(&mut &data[..])?.amounts)
}

/// ZEC/USD from MagicBlock's oracle account: (price, publish time). Only the configured feed,
/// fully verified, published at most `MAX_PRICE_AGE_SECS` before `now`.
pub fn read_price(oracle: &UncheckedAccount, now: i64) -> Result<(i64, i64)> {
    require_keys_eq!(oracle.key(), ZEC_USD_FEED, CoinflipError::BadOracle);
    require_keys_eq!(*oracle.owner, PRICE_PROGRAM, CoinflipError::BadOracle);
    let data = oracle.try_borrow_data()?;
    // Pyth PriceUpdateV2 body: write authority (8..40), verification level (40, 1 = full),
    // feed id (41..73), price (73), conf, exponent, publish time (93).
    require!(data.len() >= 101 && data[40] == 1 && data[41..73] == ZEC_USD_FEED_ID, CoinflipError::BadOracle);
    let price = i64::from_le_bytes(data[73..81].try_into().unwrap());
    let published = i64::from_le_bytes(data[93..101].try_into().unwrap());
    require!(price > 0 && now.saturating_sub(published) <= MAX_PRICE_AGE_SECS, CoinflipError::StalePrice);
    Ok((price, published))
}

/// Settles the table's open bet (see `settle_bet`). `strict`: refuse a price from before the
/// expiry (a caller retries); otherwise (the scheduled closer) just wait for a later run.
fn settle_open_bet<'info>(
    table: &mut Account<'info, Table>,
    pot: &UncheckedAccount<'info>,
    oracle: &UncheckedAccount<'info>,
    app_authority: &UncheckedAccount<'info>,
    app_authority_bump: u8,
    now: i64,
    strict: bool,
) -> Result<()> {
    let b = table.bet;
    require!(b.open, CoinflipError::NoBet);
    require!(now >= b.expiry, CoinflipError::BetNotExpired);
    if now > b.expiry.saturating_add(SETTLE_WINDOW_SECS) {
        table.bet.open = false; // void: nothing moves
        emit!(BetSettled { table: table.key(), outcome: 3, settle_price: 0 });
        return Ok(());
    }
    let (price, published) = read_price(oracle, now)?;
    if published < b.expiry {
        if strict {
            return err!(CoinflipError::PriceNotYet);
        }
        return Ok(());
    }
    let (from, to, amount, outcome) = if price == b.open_price {
        (0, 0, 0, 2u8) // tie: the stake stays
    } else if (price > b.open_price) == b.up {
        (1 - b.seat, b.seat, updown_win(table.stake), 1) // right call
    } else {
        (b.seat, 1 - b.seat, table.stake, 0) // wrong call
    };
    if amount > 0 {
        zol_ledger::cpi::move_seat(
            CpiContext::new_with_signer(
                zol_ledger::ID,
                zol_ledger::cpi::accounts::MoveSeat {
                    authority: app_authority.to_account_info(),
                    pot: pot.to_account_info(),
                    app_authority: app_authority.to_account_info(),
                },
                &[&[APP_AUTHORITY_SEED, &[app_authority_bump]]],
            ),
            from,
            to,
            amount,
        )?;
    }
    let t = table;
    t.bet.open = false;
    t.bet.settle_price = price;
    t.flips = t.flips.saturating_add(1);
    let bettor_won = outcome == 1;
    t.results = (t.results << 1) | bettor_won as u64;
    match outcome {
        1 => t.wins = t.wins.saturating_add(1),
        2 => t.ties = t.ties.saturating_add(1),
        _ => {}
    }
    t.winner = match outcome {
        1 => b.seat,
        0 => 1 - b.seat,
        _ => NO_WINNER,
    };
    t.state = TableState::Landed;
    emit!(BetSettled { table: t.key(), outcome, settle_price: price });
    Ok(())
}

/// The sealed bid a bidder commits: `sha256("ZOL/bid/v1" ‖ auction ‖ bidder ‖ amount (u64 LE) ‖ salt)`.
pub fn bid_commitment(auction: &Pubkey, bidder: &Pubkey, amount: u64, salt: &[u8; 32]) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[b"ZOL/bid/v1", auction.as_ref(), bidder.as_ref(), &amount.to_le_bytes(), salt]).to_bytes()
}

fn bidder_seat(x: &Auction, bidder: &Pubkey) -> Result<usize> {
    let n = x.seat_count as usize;
    (1..n).find(|s| x.players[*s] == *bidder).ok_or(error!(CoinflipError::NotAtTable))
}

pub fn auction_address(pot_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[AUCTION_SEED, pot_id], &crate::ID).0
}

/// `end_auction`'s return: the pot with `cash_out_auction` attached (paid from the rent
/// vault's escrow), then the auction itself; the auction PDA pays, with the fee vault.
fn close_auction_in_rollup<'info>(a: &mut EndAuction<'info>, app_authority_bump: u8, vault_bump: u8) -> Result<()> {
    use zol_ledger::state::seeds;
    let x = &a.auction;
    let pot_id = x.pot_id;
    let auction_bump = [x.bump];
    let auction_seeds: &[&[u8]] = &[AUCTION_SEED, &pot_id, &auction_bump];
    let auth_bump = [app_authority_bump];
    let auth_seeds: &[&[u8]] = &[APP_AUTHORITY_SEED, &auth_bump];
    let vb = [vault_bump];
    let vault_seeds: &[&[u8]] = &[RENT_VAULT_SEED, &vb];
    let l = &zol_ledger::ID;
    let pda = |s: &[&[u8]]| Pubkey::find_program_address(s, l).0;
    let app = pda(&[seeds::APP, crate::ID.as_ref()]);
    let n = x.seat_count as usize;
    let mut accounts = vec![
        zol_ledger::state::ActionAccount { pubkey: pda(&[seeds::CONFIG]), is_writable: false },
        zol_ledger::state::ActionAccount { pubkey: app, is_writable: false },
        zol_ledger::state::ActionAccount { pubkey: pda(&[seeds::RECEIPT, x.pot.as_ref()]), is_writable: true },
        zol_ledger::state::ActionAccount { pubkey: x.pot, is_writable: false },
        zol_ledger::state::ActionAccount { pubkey: zol_ledger::ID, is_writable: false },
    ];
    for p in &x.players[..n] {
        accounts.push(zol_ledger::state::ActionAccount { pubkey: pda(&[seeds::BALANCE, app.as_ref(), p.as_ref()]), is_writable: true });
    }
    let action = zol_ledger::state::PotAction { program: crate::ID, accounts, data: instruction::CashOutAuction {}.data(), compute_units: 300_000 };
    let payer = a.auction.to_account_info();
    let mut escrow = a.rent_vault.to_account_info();
    escrow.is_signer = true;
    zol_ledger::cpi::close_pot(
        CpiContext::new_with_signer(
            zol_ledger::ID,
            zol_ledger::cpi::accounts::ClosePot {
                payer: payer.clone(),
                authority: a.app_authority.to_account_info(),
                pot: a.pot.to_account_info(),
                magic_fee_vault: Some(a.magic_fee_vault.to_account_info()),
                escrow_authority: Some(escrow),
                magic_program: a.magic_program.to_account_info(),
                magic_context: a.magic_context.to_account_info(),
            },
            &[auth_seeds, auction_seeds, vault_seeds],
        ),
        Some(action),
    )?;
    MagicIntentBundleBuilder::new(payer, a.magic_context.to_account_info(), a.magic_program.to_account_info())
        .magic_fee_vault(a.magic_fee_vault.to_account_info())
        .commit_and_undelegate(&[a.auction.to_account_info()])
        .build_and_invoke_signed(&[auction_seeds])?;
    Ok(())
}

/// Who may finish and when: the creator or a seated player (anyone after the session
/// deadline), between flips, before the first, or after a flip the VRF never answered.
fn check_finish(t: &Table, who: Pubkey, now: i64) -> Result<()> {
    require!(who == t.creator || t.players.contains(&who) || now >= t.session_deadline, CoinflipError::NotAtTable);
    // An open bet is settled or void first (void once its window has passed: nothing moved).
    require!(!t.bet.open || now > t.bet.expiry.saturating_add(SETTLE_WINDOW_SECS), CoinflipError::BetOpen);
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
    /// Winning seats, newest in bit 0 (the last 64 flips). Up/Down: 1 when the bettor won.
    pub results: u64,
    /// `GAME_COINFLIP` or `GAME_UPDOWN`.
    pub game: u8,
    /// Up/Down: the open bet, if any.
    pub bet: BetSlot,
    /// Rounds won by seat 1 (coin flip) or by the bettor (Up/Down), and Up/Down ties.
    pub wins: u32,
    pub ties: u32,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum AuctionState {
    Registering,
    Bidding,
    Ended,
}

/// A sealed-bid auction on a ledger pot. Seat 0 is the seller. Kept on Solana after it ends:
/// the certificate (`winner`, `price`).
#[account]
#[derive(InitSpace)]
pub struct Auction {
    pub pot: Pubkey,
    pub pot_id: [u8; 32],
    /// Paid the rent; the account stays as the certificate.
    pub creator: Pubkey,
    pub validator: Pubkey,
    /// Every bidder's deposit, and the highest allowed bid.
    pub deposit: u64,
    pub register_deadline: i64,
    pub commit_deadline: i64,
    pub reveal_deadline: i64,
    pub state: AuctionState,
    pub seat_count: u8,
    /// The pot's seats in order: the seller, then bidders by registration.
    pub players: [Pubkey; AUCTION_SEATS],
    pub commits: [[u8; 32]; AUCTION_SEATS],
    /// Revealed bids.
    pub bids: [u64; AUCTION_SEATS],
    /// Bit per seat: committed, revealed.
    pub committed: u16,
    pub revealed: u16,
    /// The winning seat (`NO_WINNER` if none) and the price paid.
    pub winner: u8,
    pub price: u64,
    pub bump: u8,
}

/// An Up/Down bet: the bettor's seat, their call, the price at the bet and when it expires.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct BetSlot {
    pub open: bool,
    pub up: bool,
    pub seat: u8,
    pub open_price: i64,
    pub expiry: i64,
    /// The price it settled at (0 before, or when void).
    pub settle_price: i64,
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

#[derive(Accounts)]
#[instruction(pot_id: [u8; 32])]
pub struct CreateAuction<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// The seller's session key (seat 0).
    pub seller: Signer<'info>,
    #[account(init, payer = payer, space = 8 + Auction::INIT_SPACE, seeds = [AUCTION_SEED, pot_id.as_ref()], bump)]
    pub auction: Box<Account<'info, Auction>>,
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
    /// CHECK: the seller's ledger balance; checked by the ledger
    #[account(mut)]
    pub seller_balance: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterBidder<'info> {
    pub session_key: Signer<'info>,
    #[account(mut, seeds = [AUCTION_SEED, auction.pot_id.as_ref()], bump = auction.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub auction: Box<Account<'info, Auction>>,
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
    /// CHECK: checked by `has_one` and the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
}

#[delegate]
#[derive(Accounts)]
pub struct StartAuction<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the auction PDA; untyped because it changes owner in this instruction
    #[account(mut, del)]
    pub auction: UncheckedAccount<'info>,
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
    /// CHECK: checked against the auction and by the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: ledger's delegation buffer for the pot
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
}

#[derive(Accounts)]
pub struct AuctionBid<'info> {
    pub bidder: Signer<'info>,
    #[account(mut, seeds = [AUCTION_SEED, auction.pot_id.as_ref()], bump = auction.bump)]
    pub auction: Box<Account<'info, Auction>>,
}

/// No signer: the scheduled task (or anyone) runs it.
#[commit]
#[derive(Accounts)]
pub struct EndAuction<'info> {
    #[account(mut, seeds = [AUCTION_SEED, auction.pot_id.as_ref()], bump = auction.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub auction: Box<Account<'info, Auction>>,
    /// CHECK: checked by `has_one` and the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    /// CHECK: escrow authority of the pot's return action
    #[account(seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
    /// CHECK: the validator's magic fee vault (checked in the handler)
    #[account(mut)]
    pub magic_fee_vault: UncheckedAccount<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct ScheduleAuctionEnd<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [AUCTION_SEED, auction.pot_id.as_ref()], bump = auction.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub auction: Box<Account<'info, Auction>>,
    /// CHECK: checked by `has_one`
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub app_authority: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
    /// CHECK: escrow authority of the pot's return action
    #[account(seeds = [RENT_VAULT_SEED], bump)]
    pub rent_vault: UncheckedAccount<'info>,
    /// CHECK: only named in the scheduled instruction
    pub magic_fee_vault: UncheckedAccount<'info>,
}

/// Balances of every seat follow as remaining accounts, in seat order.
#[derive(Accounts)]
pub struct CashOutAuction<'info> {
    /// CHECK: ledger Config; checked by the ledger
    pub config: UncheckedAccount<'info>,
    /// CHECK: ledger App; checked by the ledger
    pub app: UncheckedAccount<'info>,
    /// CHECK: owner checked here, the rest by the ledger
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,
    /// CHECK: checked by the ledger
    pub pot: UncheckedAccount<'info>,
    /// CHECK: the ledger
    #[account(address = zol_ledger::ID)]
    pub ledger_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Bet<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: the table's pot (`has_one`); owner checked when read
    pub pot: UncheckedAccount<'info>,
    /// CHECK: the ZEC/USD oracle account; address, owner and contents checked in `read_price`
    pub oracle: UncheckedAccount<'info>,
}

/// No signer: anyone may settle (the page, the house, the scheduled closer).
#[derive(Accounts)]
pub struct SettleBet<'info> {
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump, has_one = pot @ CoinflipError::BadAccount)]
    pub table: Account<'info, Table>,
    /// CHECK: checked by `has_one` and the ledger
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: the ZEC/USD oracle account; checked in `read_price`
    pub oracle: UncheckedAccount<'info>,
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
    /// CHECK: the ZEC/USD oracle account, to settle an Up/Down bet; checked when read
    pub oracle: UncheckedAccount<'info>,
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
    /// CHECK: the ZEC/USD oracle account; only named in the scheduled instruction
    #[account(address = ZEC_USD_FEED)]
    pub oracle: UncheckedAccount<'info>,
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
pub struct AuctionCreated {
    pub auction: Pubkey,
    pub deposit: u64,
    pub register_deadline: i64,
    pub commit_deadline: i64,
    pub reveal_deadline: i64,
}
#[event]
pub struct Registered {
    pub auction: Pubkey,
    pub bidder: Pubkey,
    pub seat: u8,
}
#[event]
pub struct AuctionStarted {
    pub auction: Pubkey,
    pub bidders: u8,
}
#[event]
pub struct AuctionEnded {
    pub auction: Pubkey,
    pub winner: Option<Pubkey>,
    pub price: u64,
    pub revealed: u8,
    pub forfeited: u8,
}
#[event]
pub struct BetPlaced {
    pub table: Pubkey,
    pub up: bool,
    pub open_price: i64,
    pub expiry: i64,
}
/// `outcome`: 0 the bettor lost, 1 won, 2 tie, 3 void (not settled within the window).
#[event]
pub struct BetSettled {
    pub table: Pubkey,
    pub outcome: u8,
    pub settle_price: i64,
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
    #[msg("not this table's game")]
    WrongGame,
    #[msg("a bet is open")]
    BetOpen,
    #[msg("no open bet")]
    NoBet,
    #[msg("the bet has not expired yet")]
    BetNotExpired,
    #[msg("no price published since the bet expired yet; try again in a second")]
    PriceNotYet,
    #[msg("not the ZEC/USD oracle account")]
    BadOracle,
    #[msg("the oracle price is too old")]
    StalePrice,
    #[msg("auction terms out of bounds")]
    BadAuction,
    #[msg("not this phase of the auction")]
    AuctionPhase,
    #[msg("no sealed bid to reveal")]
    NoCommitment,
    #[msg("a bid must be positive and at most the deposit")]
    BadBid,
    #[msg("the amount and salt do not match the sealed bid")]
    BadReveal,
}
