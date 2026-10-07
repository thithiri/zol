//! ZOL kernel ledger (design v0.3, "Ledger program").
//!
//! Records who is owed what for ZEC held in per-app Zcash vaults. Apps are
//! untrusted callers; the ledger never calls app code. Value enters only through
//! attested credits and leaves only through settlement records.

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::ephemeral;

pub mod error;
pub mod events;
pub mod instructions;
pub mod sig;
pub mod state;

pub use instructions::*;
pub use state::*;

declare_id!("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y");

#[ephemeral]
#[program]
pub mod zol_ledger {
    use super::*;

    // Configuration and app lifecycle

    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        admin::handle_initialize(ctx, args)
    }

    pub fn set_config(ctx: Context<AdminOnly>, args: SetConfigArgs) -> Result<()> {
        admin::handle_set_config(ctx, args)
    }

    pub fn set_guardians<'info>(
        ctx: Context<'info, GuardianQuorum<'info>>,
        guardians: Vec<Pubkey>,
        threshold: u8,
    ) -> Result<()> {
        admin::handle_set_guardians(ctx, guardians, threshold)
    }

    pub fn set_cap(ctx: Context<SetCap>, cap: u64) -> Result<()> {
        admin::handle_set_cap(ctx, cap)
    }

    pub fn register_app(ctx: Context<RegisterApp>) -> Result<()> {
        admin::handle_register_app(ctx)
    }

    pub fn activate_app(ctx: Context<ActivateApp>, vault_address: [u8; 43], genesis_head: [u8; 32]) -> Result<()> {
        admin::handle_activate_app(ctx, vault_address, genesis_head)
    }

    pub fn freeze_app(ctx: Context<FreezeApp>) -> Result<()> {
        admin::handle_freeze_app(ctx)
    }

    pub fn unfreeze_app<'info>(ctx: Context<'info, UnfreezeApp<'info>>) -> Result<()> {
        admin::handle_unfreeze_app(ctx)
    }

    // Deposits

    pub fn credit_deposit(ctx: Context<CreditDeposit>, args: CreditArgs) -> Result<()> {
        deposit::handle_credit_deposit(ctx, args)
    }

    pub fn void_deposit(ctx: Context<VoidDeposit>, deposit_id: [u8; 32]) -> Result<()> {
        deposit::handle_void_deposit(ctx, deposit_id)
    }

    // Pots

    pub fn open_pot(ctx: Context<OpenPot>, pot_id: [u8; 32], open_deadline: i64) -> Result<()> {
        pot::handle_open_pot(ctx, pot_id, open_deadline)
    }

    pub fn buy_in(ctx: Context<BuyIn>, amount: u64) -> Result<()> {
        pot::handle_buy_in(ctx, amount)
    }

    pub fn leave_pot(ctx: Context<LeavePot>) -> Result<()> {
        pot::handle_leave_pot(ctx)
    }

    pub fn delegate_pot(
        ctx: Context<DelegatePot>,
        expected: Vec<SeatStake>,
        session_deadline: i64,
        validator: Pubkey,
    ) -> Result<()> {
        pot::handle_delegate_pot(ctx, expected, session_deadline, validator)
    }

    pub fn move_seat(ctx: Context<MoveSeat>, from: u8, to: u8, amount: u64) -> Result<()> {
        pot::handle_move_seat(ctx, from, to, amount)
    }

    pub fn close_pot(ctx: Context<ClosePot>) -> Result<()> {
        pot::handle_close_pot(ctx)
    }

    pub fn cash_out<'info>(ctx: Context<'info, PotOutcome<'info>>) -> Result<()> {
        pot::handle_cash_out(ctx)
    }

    pub fn refund_pot<'info>(ctx: Context<'info, PotOutcome<'info>>) -> Result<()> {
        pot::handle_refund_pot(ctx)
    }

    // Exit

    pub fn request_exit(ctx: Context<RequestExit>) -> Result<()> {
        exit::handle_request_exit(ctx)
    }

    pub fn settle<'info>(ctx: Context<'info, Settle<'info>>) -> Result<()> {
        exit::handle_settle(ctx)
    }

    pub fn mark_paid(ctx: Context<MarkPaid>) -> Result<()> {
        exit::handle_mark_paid(ctx)
    }
}
