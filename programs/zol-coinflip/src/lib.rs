//! zol-coinflip: the flagship ZOL app. A two-player coin flip on a ledger pot.
//!
//! | Instruction    | Runs on | What it does |
//! |----------------|---------|--------------|
//! | `register`     | Solana  | One-off: registers this program with the ledger (CPI `register_app`) |
//! | `create_table` | Solana  | Opens a ledger pot (CPI `open_pot`) and a `Table` for it, naming the rollup validator |
//! | `join`         | Solana  | CPI `buy_in` for the table stake; the player's session key signs |
//! | `start`        | Solana  | CPI `delegate_pot` with the receipt's stakes, and delegates the table, both to the table's validator |
//! | `flip`         | Rollup  | Requests MagicBlock VRF randomness; the callback is `land` |
//! | `land`         | Rollup  | VRF callback only: CPI `move_seat` so the winner's seat holds the whole pot |
//! | `finish`       | Rollup  | CPI `close_pot` and commits and undelegates the table |
//! | `close_table`  | Solana  | Anyone, once the ledger's receipt is terminal: closes the table, rent to its creator |
//!
//! `cash_out` is the ledger's and anyone may call it once the pot is back.
//!
//! The app stores no player keys: who sits where is read from the ledger's
//! `PotReceipt`, which is the source of truth. The app signs every ledger call
//! with its `["zol-app"]` address.

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{commit, delegate, ephemeral, vrf, vrf_callback};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::MagicIntentBundleBuilder;
use ephemeral_rollups_sdk::vrf::instructions::{create_request_randomness_ix, RequestRandomnessParams};
use ephemeral_rollups_sdk::vrf::types::SerializableAccountMeta;
use zol_ledger::state::{bps, Config, Pot, PotReceipt, PotState, SeatStake};

declare_id!("7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo");

pub const APP_AUTHORITY_SEED: &[u8] = b"zol-app";
pub const TABLE_SEED: &[u8] = b"table";
pub const SEATS: u8 = 2;
pub const NO_WINNER: u8 = u8::MAX;

/// Deposit denominations in zatoshi. Must match the gateway's `denominations`
/// (gateway/src/config.rs default). A table's stake is one deposit's net credited
/// value, `d - floor(d * entry_fee_bps / 10_000)`, so a single deposit buys one seat.
pub const STAKE_DENOMINATIONS: [u64; 2] = [1_000_000, 10_000_000];

/// The stakes `create_table` accepts under an entry fee of `entry_fee_bps`.
pub fn allowed_stakes(entry_fee_bps: u16) -> [u64; 2] {
    STAKE_DENOMINATIONS.map(|d| d - bps(d, entry_fee_bps))
}

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
        let fee_bps = {
            let info = &ctx.accounts.config;
            require_keys_eq!(*info.owner, zol_ledger::ID, CoinflipError::BadAccount);
            let (expected, _) = Pubkey::find_program_address(&[zol_ledger::state::seeds::CONFIG], &zol_ledger::ID);
            require_keys_eq!(info.key(), expected, CoinflipError::BadAccount);
            let data = info.try_borrow_data()?;
            Config::try_deserialize(&mut &data[..])?.params.entry_fee_bps
        };
        require!(allowed_stakes(fee_bps).contains(&stake), CoinflipError::BadStake);
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
        emit!(TableCreated { table: t.key(), pot: t.pot, stake, validator });
        Ok(())
    }

    /// The player's session key signs; the ledger moves the stake from their balance.
    pub fn join(ctx: Context<Join>) -> Result<()> {
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
            a.table.stake,
        )?;
        emit!(Joined { table: a.table.key(), player, seat: r.seat_count });
        Ok(())
    }

    /// Anyone, once both seats hold the stake. Sends the pot and the table to the
    /// table's validator.
    pub fn start(ctx: Context<Start>, session_secs: i64) -> Result<()> {
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
        require!(expected.iter().all(|s| s.amount == table.stake), CoinflipError::BadStake);

        // Write the table before it changes owner.
        table.state = TableState::Started;
        {
            let mut data = table_info.try_borrow_mut_data()?;
            let mut w: &mut [u8] = &mut data[..];
            table.try_serialize(&mut w)?;
        }

        let a = &ctx.accounts;
        let session_deadline = now.checked_add(session_secs).ok_or(CoinflipError::Overflow)?;
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
        a.delegate_table(
            &a.payer,
            &[TABLE_SEED, &table.pot_id],
            DelegateConfig { validator: Some(validator), ..Default::default() },
        )?;
        emit!(Started { table: table_info.key(), validator, session_deadline });
        Ok(())
    }

    /// Rollup. Anyone may trigger the flip of a started table; the outcome is the oracle's.
    pub fn flip(ctx: Context<Flip>, caller_seed: [u8; 32]) -> Result<()> {
        require!(ctx.accounts.table.state == TableState::Started, CoinflipError::BadTableState);
        require!(ALLOWED_QUEUES.contains(&ctx.accounts.oracle_queue.key()), CoinflipError::BadQueue);
        ctx.accounts.table.state = TableState::Flipping;
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
        let amount = pot.amounts[loser as usize];
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
        emit!(Landed { table: t.key(), winner, amount });
        Ok(())
    }

    /// Rollup. Anyone, after the coin landed: returns the pot (ledger `close_pot`)
    /// and the table to Solana.
    pub fn finish(ctx: Context<Finish>) -> Result<()> {
        require!(ctx.accounts.table.state == TableState::Landed, CoinflipError::BadTableState);
        ctx.accounts.table.state = TableState::Finished;
        ctx.accounts.table.exit(&crate::ID)?;
        let a = &ctx.accounts;
        let bump = ctx.bumps.app_authority;
        zol_ledger::cpi::close_pot(CpiContext::new_with_signer(
            zol_ledger::ID,
            zol_ledger::cpi::accounts::ClosePot {
                payer: a.payer.to_account_info(),
                authority: a.app_authority.to_account_info(),
                pot: a.pot.to_account_info(),
                magic_program: a.magic_program.to_account_info(),
                magic_context: a.magic_context.to_account_info(),
            },
            &[&[APP_AUTHORITY_SEED, &[bump]]],
        ))?;
        MagicIntentBundleBuilder::new(
            a.payer.to_account_info(),
            a.magic_context.to_account_info(),
            a.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[a.table.to_account_info()])
        .build_and_invoke()?;
        emit!(Finished { table: a.table.key(), winner: a.table.winner });
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
}

#[vrf]
#[derive(Accounts)]
pub struct Flip<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [TABLE_SEED, table.pot_id.as_ref()], bump = table.bump)]
    pub table: Account<'info, Table>,
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
pub struct TableClosed {
    pub table: Pubkey,
    pub creator: Pubkey,
    pub pot_state: u8,
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
}
