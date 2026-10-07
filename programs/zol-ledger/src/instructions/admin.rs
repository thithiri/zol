//! Configuration and app lifecycle: initialize, set_config, set_guardians, set_cap,
//! register_app, activate_app, freeze_app, unfreeze_app.

use anchor_lang::prelude::*;

use crate::error::ZolError;
use crate::events::*;
use crate::program::ZolLedger;
use crate::state::{seeds, *};

// ---------------------------------------------------------------- initialize

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(init, payer = payer, space = 8 + Config::INIT_SPACE, seeds = [seeds::CONFIG], bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ ZolError::NotUpgradeAuthority)]
    pub program: Program<'info, ZolLedger>,
    #[account(constraint = program_data.upgrade_authority_address == Some(payer.key()) @ ZolError::NotUpgradeAuthority)]
    pub program_data: Account<'info, ProgramData>,
    pub system_program: Program<'info, System>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeArgs {
    pub admin: Pubkey,
    pub attestor: Pubkey,
    pub guardians: Vec<Pubkey>,
    pub guardian_threshold: u8,
    pub network: u8,
    pub params: Params,
}

pub fn handle_initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
    args.params.validate()?;
    validate_guardians(&args.guardians, args.guardian_threshold)?;
    require!(zol_wire::Network::from_u8(args.network).is_some(), ZolError::BadParams);
    let c = &mut ctx.accounts.config;
    c.admin = args.admin;
    c.attestor = args.attestor;
    c.guardians = [Pubkey::default(); limits::MAX_GUARDIANS];
    c.guardians[..args.guardians.len()].copy_from_slice(&args.guardians);
    c.guardian_count = args.guardians.len() as u8;
    c.guardian_threshold = args.guardian_threshold;
    c.network = args.network;
    c.paused = false;
    c.params = args.params;
    c.bump = ctx.bumps.config;
    Ok(())
}

// ---------------------------------------------------------------- set_config

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [seeds::CONFIG], bump = config.bump, has_one = admin)]
    pub config: Box<Account<'info, Config>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetConfigArgs {
    pub new_admin: Option<Pubkey>,
    pub attestor: Option<Pubkey>,
    pub params: Option<Params>,
    pub paused: Option<bool>,
}

/// The admin can change parameters within the program's caps, rotate the attestor
/// key, and pause. It cannot change guardians and cannot unfreeze.
pub fn handle_set_config(ctx: Context<AdminOnly>, args: SetConfigArgs) -> Result<()> {
    let c = &mut ctx.accounts.config;
    if let Some(p) = args.params {
        p.validate()?;
        c.params = p;
    }
    if let Some(a) = args.attestor {
        c.attestor = a;
    }
    if let Some(p) = args.paused {
        c.paused = p;
    }
    if let Some(a) = args.new_admin {
        c.admin = a;
    }
    emit!(ConfigChanged { admin: c.admin, attestor: c.attestor, paused: c.paused });
    Ok(())
}

// ---------------------------------------------------------------- set_guardians

#[derive(Accounts)]
pub struct GuardianQuorum<'info> {
    #[account(mut, seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
}

/// Replaces the guardian set. Needs a threshold of current guardians as signers
/// in the remaining accounts.
pub fn handle_set_guardians<'info>(ctx: Context<'info, GuardianQuorum<'info>>, guardians: Vec<Pubkey>, threshold: u8) -> Result<()> {
    let c = &mut ctx.accounts.config;
    require!(
        c.count_guardian_signers(ctx.remaining_accounts) >= c.guardian_threshold as usize,
        ZolError::GuardianThreshold
    );
    validate_guardians(&guardians, threshold)?;
    c.guardians = [Pubkey::default(); limits::MAX_GUARDIANS];
    c.guardians[..guardians.len()].copy_from_slice(&guardians);
    c.guardian_count = guardians.len() as u8;
    c.guardian_threshold = threshold;
    Ok(())
}

// ---------------------------------------------------------------- set_cap

#[derive(Accounts)]
pub struct SetCap<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump, has_one = admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
}

pub fn handle_set_cap(ctx: Context<SetCap>, cap: u64) -> Result<()> {
    ctx.accounts.app.deposit_cap = cap;
    Ok(())
}

// ---------------------------------------------------------------- register_app

#[derive(Accounts)]
pub struct RegisterApp<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: must be an executable program; its zol-app address must sign.
    #[account(constraint = app_program.executable @ ZolError::NotAProgram)]
    pub app_program: UncheckedAccount<'info>,
    /// The app's consent: only the app program can sign for this address.
    pub app_authority: Signer<'info>,
    #[account(init, payer = payer, space = 8 + App::INIT_SPACE, seeds = [seeds::APP, app_program.key().as_ref()], bump)]
    pub app: Box<Account<'info, App>>,
    pub system_program: Program<'info, System>,
}

pub fn handle_register_app(ctx: Context<RegisterApp>) -> Result<()> {
    let (expected, _) = Pubkey::find_program_address(&[seeds::APP_AUTHORITY], ctx.accounts.app_program.key);
    require_keys_eq!(ctx.accounts.app_authority.key(), expected, ZolError::BadAppAuthority);
    let bond = ctx.accounts.config.params.registration_bond;
    if bond > 0 {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.payer.to_account_info(),
                    to: ctx.accounts.app.to_account_info(),
                },
            ),
            bond,
        )?;
    }
    let app = &mut ctx.accounts.app;
    app.program_id = ctx.accounts.app_program.key();
    app.authority = expected;
    app.registrant = ctx.accounts.payer.key();
    app.state = AppState::Pending;
    app.bond = bond;
    app.bump = ctx.bumps.app;
    emit!(AppRegistered { app: app.program_id, authority: app.authority });
    Ok(())
}

// ---------------------------------------------------------------- activate_app

#[derive(Accounts)]
pub struct ActivateApp<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump, has_one = admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
}

/// Records the vault address and genesis head. They take effect after the
/// activation delay unless a guardian cancels, and never change once active.
pub fn handle_activate_app(ctx: Context<ActivateApp>, vault_address: [u8; 43], genesis_head: [u8; 32]) -> Result<()> {
    let app = &mut ctx.accounts.app;
    require!(app.state == AppState::Pending, ZolError::BadAppState);
    require!(vault_address != [0u8; 43] && genesis_head != [0u8; 32], ZolError::BadParams);
    let now = Clock::get()?.unix_timestamp;
    app.vault_address = vault_address;
    app.genesis_head = genesis_head;
    app.state = AppState::Activating;
    app.activates_at = now
        .checked_add(ctx.accounts.config.params.activation_delay_secs)
        .ok_or(ZolError::Overflow)?;
    emit!(AppActivating { app: app.program_id, vault_address, genesis_head, activates_at: app.activates_at });
    Ok(())
}

// ---------------------------------------------------------------- freeze_app

#[derive(Accounts)]
pub struct FreezeApp<'info> {
    pub guardian: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump, constraint = config.is_guardian(guardian.key) @ ZolError::NotGuardian)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
}

/// Any one guardian: freezes an active app, or cancels an activation still in its delay.
pub fn handle_freeze_app(ctx: Context<FreezeApp>) -> Result<()> {
    let app = &mut ctx.accounts.app;
    app.refresh(Clock::get()?.unix_timestamp);
    match app.state {
        AppState::Activating => {
            app.state = AppState::Pending;
            app.vault_address = [0; 43];
            app.genesis_head = [0; 32];
            app.activates_at = 0;
            emit!(ActivationCancelled { app: app.program_id, guardian: ctx.accounts.guardian.key() });
        }
        AppState::Active => {
            app.state = AppState::Frozen;
            emit!(AppFrozen { app: app.program_id, guardian: ctx.accounts.guardian.key() });
        }
        _ => return err!(ZolError::BadAppState),
    }
    Ok(())
}

// ---------------------------------------------------------------- unfreeze_app

#[derive(Accounts)]
pub struct UnfreezeApp<'info> {
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
}

/// A threshold of guardians (signers in the remaining accounts) returns a frozen app to Active.
pub fn handle_unfreeze_app<'info>(ctx: Context<'info, UnfreezeApp<'info>>) -> Result<()> {
    let c = &ctx.accounts.config;
    require!(
        c.count_guardian_signers(ctx.remaining_accounts) >= c.guardian_threshold as usize,
        ZolError::GuardianThreshold
    );
    let app = &mut ctx.accounts.app;
    require!(app.state == AppState::Frozen, ZolError::BadAppState);
    app.state = AppState::Active;
    emit!(AppUnfrozen { app: app.program_id });
    Ok(())
}
