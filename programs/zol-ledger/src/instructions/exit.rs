//! Exit: request_exit, settle, mark_paid.

use anchor_lang::prelude::*;
use zol_wire::digest::paid_digest;

use crate::error::ZolError;
use crate::events::*;
use crate::sig::{require_ed25519, SolSha256};
use crate::state::{seeds, *};

#[derive(Accounts)]
pub struct RequestExit<'info> {
    pub session_key: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::BALANCE, balance.app.as_ref(), session_key.key().as_ref()], bump = balance.bump)]
    pub balance: Box<Account<'info, Balance>>,
}

/// Marks a balance of at least the minimum exit for payout. The whole balance at
/// `settle` time is paid; buy-ins are refused while it is marked.
pub fn handle_request_exit(ctx: Context<RequestExit>) -> Result<()> {
    let b = &mut ctx.accounts.balance;
    require!(b.amount >= ctx.accounts.config.params.min_exit, ZolError::BelowMinExit);
    b.exiting = true;
    emit!(ExitRequested { app: b.app, session_key: b.session_key, amount: b.amount });
    Ok(())
}

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
    #[account(
        init, payer = payer, space = 8 + Settlement::INIT_SPACE,
        seeds = [seeds::SETTLEMENT, app.key().as_ref(), app.next_settlement.to_le_bytes().as_ref()], bump
    )]
    pub settlement: Box<Account<'info, Settlement>>,
    pub system_program: Program<'info, System>,
}

/// Anyone. Turns up to 16 exiting balances (remaining accounts) into the app's
/// next settlement record. Each payout is the balance less the exit fee and the
/// network fee allowance; both deductions join the fee pool.
pub fn handle_settle<'info>(ctx: Context<'info, Settle<'info>>) -> Result<()> {
    let p = ctx.accounts.config.params;
    let app = &mut ctx.accounts.app;
    app.refresh(Clock::get()?.unix_timestamp);
    app.require_active(&ctx.accounts.config)?;
    let balances = ctx.remaining_accounts;
    require!(!balances.is_empty() && balances.len() <= limits::MAX_PAYOUTS, ZolError::BadPayoutCount);

    let s = &mut ctx.accounts.settlement;
    let mut total: u64 = 0;
    for (i, info) in balances.iter().enumerate() {
        require!(!balances[..i].iter().any(|b| b.key == info.key), ZolError::DuplicateAccount);
        let mut b = Account::<Balance>::try_from(info)?;
        require!(b.app == app.key(), ZolError::BadAccount);
        require!(b.exiting, ZolError::NotExiting);
        require!(b.amount >= p.min_exit, ZolError::BelowMinExit);
        let amount = b.amount;
        let fee = bps(amount, p.exit_fee_bps)
            .checked_add(p.network_fee_allowance)
            .ok_or(ZolError::Overflow)?;
        let payout = amount.checked_sub(fee).ok_or(ZolError::BelowMinExit)?;
        s.payouts[i] = PayoutEntry { commitment: b.payout_commitment, amount: payout };
        total = total.checked_add(payout).ok_or(ZolError::Overflow)?;
        app.liability = app.liability.checked_sub(amount).ok_or(ZolError::Overflow)?;
        app.fee_pool = app.fee_pool.checked_add(fee).ok_or(ZolError::Overflow)?;
        b.amount = 0;
        b.exiting = false;
        b.exit(&crate::ID)?;
    }
    s.app = app.key();
    s.sequence = app.next_settlement;
    s.state = SettlementState::Unpaid;
    s.count = balances.len() as u8;
    s.total = total;
    s.bump = ctx.bumps.settlement;
    app.next_settlement = app.next_settlement.checked_add(1).ok_or(ZolError::Overflow)?;
    app.unpaid_total = app.unpaid_total.checked_add(total).ok_or(ZolError::Overflow)?;
    emit!(Settled { app: app.program_id, sequence: s.sequence, count: s.count, total });
    Ok(())
}

#[derive(Accounts)]
pub struct MarkPaid<'info> {
    pub relayer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
    #[account(
        mut, seeds = [seeds::SETTLEMENT, app.key().as_ref(), settlement.sequence.to_le_bytes().as_ref()],
        bump = settlement.bump
    )]
    pub settlement: Box<Account<'info, Settlement>>,
    /// CHECK: instructions sysvar
    #[account(address = crate::sig::INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
}

/// Public record for auditors, posted after payout depth. Signers never rely on it.
pub fn handle_mark_paid(ctx: Context<MarkPaid>) -> Result<()> {
    let config = &ctx.accounts.config;
    let app = &mut ctx.accounts.app;
    let s = &mut ctx.accounts.settlement;
    require!(s.state == SettlementState::Unpaid, ZolError::AlreadyPaid);
    require!(s.sequence == app.paid_count, ZolError::OutOfOrder);
    let digest = paid_digest::<SolSha256>(config.network, &app.program_id.to_bytes(), s.sequence);
    require_ed25519(&ctx.accounts.instructions, &config.attestor, &digest)?;
    s.state = SettlementState::Paid;
    app.paid_count += 1;
    app.unpaid_total = app.unpaid_total.checked_sub(s.total).ok_or(ZolError::Overflow)?;
    emit!(SettlementPaid { app: app.program_id, sequence: s.sequence });
    Ok(())
}
