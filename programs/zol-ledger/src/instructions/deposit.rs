//! credit_deposit and void_deposit. Both create the same `Deposit` account, so
//! Solana admits exactly one of them per deposit ID.

use anchor_lang::prelude::*;
use zol_wire::digest::void_digest;
use zol_wire::memo::FLAGS_KNOWN;
use zol_wire::CreditAttestation;

use crate::error::ZolError;
use crate::events::*;
use crate::sig::{require_ed25519, SolSha256};
use crate::state::{seeds, *};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreditArgs {
    pub deposit_id: [u8; 32],
    pub amount: u64,
    pub session_key: Pubkey,
    pub payout_commitment: [u8; 32],
    pub flags: u8,
    pub pot_id: [u8; 32],
    pub app_data: Vec<u8>,
}

#[derive(Accounts)]
#[instruction(args: CreditArgs)]
pub struct CreditDeposit<'info> {
    #[account(mut)]
    pub relayer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
    #[account(init, payer = relayer, space = 8 + Deposit::INIT_SPACE, seeds = [seeds::DEPOSIT, args.deposit_id.as_ref()], bump)]
    pub deposit: Box<Account<'info, Deposit>>,
    #[account(
        init_if_needed, payer = relayer, space = 8 + Balance::INIT_SPACE,
        seeds = [seeds::BALANCE, app.key().as_ref(), args.session_key.as_ref()], bump
    )]
    pub balance: Box<Account<'info, Balance>>,
    /// CHECK: instructions sysvar
    #[account(address = crate::sig::INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_credit_deposit(ctx: Context<CreditDeposit>, args: CreditArgs) -> Result<()> {
    let config = &ctx.accounts.config;
    let app = &mut ctx.accounts.app;
    app.refresh(Clock::get()?.unix_timestamp);
    app.require_active(config)?;
    require!(args.amount > 0, ZolError::ZeroAmount);
    require!(config.params.is_denomination(args.amount), ZolError::BadDenomination);
    require!(args.flags & !FLAGS_KNOWN == 0, ZolError::BadFlags);
    require!(args.app_data.len() <= limits::MAX_APP_DATA, ZolError::AppDataTooLong);

    let digest = CreditAttestation {
        network: config.network,
        app: app.program_id.to_bytes(),
        deposit_id: args.deposit_id,
        amount: args.amount,
        session_key: args.session_key.to_bytes(),
        payout_commitment: args.payout_commitment,
        flags: args.flags,
        pot_id: args.pot_id,
        app_data: &args.app_data,
    }
    .digest::<SolSha256>();
    require_ed25519(&ctx.accounts.instructions, &config.attestor, &digest)?;

    let fee = bps(args.amount, config.params.entry_fee_bps);
    let net = args.amount - fee;
    let exposure = app
        .liability
        .checked_add(app.unpaid_total)
        .and_then(|v| v.checked_add(net))
        .ok_or(ZolError::Overflow)?;
    require!(exposure <= app.deposit_cap, ZolError::OverCap);

    let index = app.deposit_count;
    app.deposit_count = index.checked_add(1).ok_or(ZolError::Overflow)?;
    app.credited_total = app.credited_total.checked_add(args.amount).ok_or(ZolError::Overflow)?;
    app.liability = app.liability.checked_add(net).ok_or(ZolError::Overflow)?;
    app.fee_pool = app.fee_pool.checked_add(fee).ok_or(ZolError::Overflow)?;

    let d = &mut ctx.accounts.deposit;
    d.id = args.deposit_id;
    d.state = DepositState::Credited;
    d.app = app.key();
    d.index = index;
    d.amount = args.amount;
    d.session_key = args.session_key;
    d.payout_commitment = args.payout_commitment;
    d.pot_id = args.pot_id;
    d.digest = digest;
    d.bump = ctx.bumps.deposit;

    let b = &mut ctx.accounts.balance;
    if b.app == Pubkey::default() {
        b.app = app.key();
        b.session_key = args.session_key;
        b.payout_commitment = args.payout_commitment;
        b.bump = ctx.bumps.balance;
    } else {
        // A later deposit to the same session must name the same payout address.
        require!(b.payout_commitment == args.payout_commitment, ZolError::PayoutCommitmentMismatch);
    }
    b.amount = b.amount.checked_add(net).ok_or(ZolError::Overflow)?;
    b.auto_exit = args.flags & zol_wire::memo::FLAG_AUTO_EXIT != 0;

    emit!(DepositCredited {
        app: app.program_id,
        deposit_id: args.deposit_id,
        index,
        amount: args.amount,
        fee,
        session_key: args.session_key,
    });
    Ok(())
}

#[derive(Accounts)]
#[instruction(deposit_id: [u8; 32])]
pub struct VoidDeposit<'info> {
    #[account(mut)]
    pub relayer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
    #[account(init, payer = relayer, space = 8 + Deposit::INIT_SPACE, seeds = [seeds::DEPOSIT, deposit_id.as_ref()], bump)]
    pub deposit: Box<Account<'info, Deposit>>,
    /// CHECK: instructions sysvar
    #[account(address = crate::sig::INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Records a void in any app state, so a deposit to a frozen, paused or cancelled
/// vault can always be refunded. Credits nothing.
pub fn handle_void_deposit(ctx: Context<VoidDeposit>, deposit_id: [u8; 32]) -> Result<()> {
    let config = &ctx.accounts.config;
    let app = &ctx.accounts.app;
    let digest = void_digest::<SolSha256>(config.network, &app.program_id.to_bytes(), &deposit_id);
    require_ed25519(&ctx.accounts.instructions, &config.attestor, &digest)?;
    let d = &mut ctx.accounts.deposit;
    d.id = deposit_id;
    d.state = DepositState::Voided;
    d.app = app.key();
    d.digest = digest;
    d.bump = ctx.bumps.deposit;
    emit!(DepositVoided { app: app.program_id, deposit_id });
    Ok(())
}
