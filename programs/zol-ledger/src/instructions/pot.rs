//! Pots: open_pot, buy_in, leave_pot, delegate_pot (Solana); move_seat, close_pot
//! (rollup); cash_out, refund_pot (Solana).
//!
//! Player keys live only in the `PotReceipt`, which never leaves Solana. The
//! delegated `Pot` carries one amount per seat in receipt order.

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{commit, delegate};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::{CallHandler, FoldableIntentBuilder, MagicIntentBundleBuilder};
use ephemeral_rollups_sdk::{ActionArgs, ShortAccountMeta};

use crate::error::ZolError;
use crate::events::*;
use crate::state::{seeds, *};

// ---------------------------------------------------------------- open_pot

#[derive(Accounts)]
#[instruction(pot_id: [u8; 32])]
pub struct OpenPot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump, has_one = authority @ ZolError::BadAppAuthority)]
    pub app: Box<Account<'info, App>>,
    pub authority: Signer<'info>,
    #[account(init, payer = payer, space = 8 + Pot::INIT_SPACE, seeds = [seeds::POT, app.key().as_ref(), pot_id.as_ref()], bump)]
    pub pot: Box<Account<'info, Pot>>,
    #[account(init, payer = payer, space = 8 + PotReceipt::INIT_SPACE, seeds = [seeds::RECEIPT, pot.key().as_ref()], bump)]
    pub receipt: Box<Account<'info, PotReceipt>>,
    pub system_program: Program<'info, System>,
}

pub fn handle_open_pot(ctx: Context<OpenPot>, pot_id: [u8; 32], open_deadline: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let app = &mut ctx.accounts.app;
    app.refresh(now);
    app.require_active(&ctx.accounts.config)?;
    let max = now.checked_add(ctx.accounts.config.params.max_open_secs).ok_or(ZolError::Overflow)?;
    require!(open_deadline > now && open_deadline <= max, ZolError::BadDeadline);

    let pot = &mut ctx.accounts.pot;
    pot.app = app.key();
    pot.app_authority = app.authority;
    pot.pot_id = pot_id;
    pot.bump = ctx.bumps.pot;

    let r = &mut ctx.accounts.receipt;
    r.app = app.key();
    r.pot = pot.key();
    r.pot_id = pot_id;
    r.state = PotState::Open;
    r.open_deadline = open_deadline;
    r.bump = ctx.bumps.receipt;
    emit!(PotOpened { app: app.program_id, pot: pot.key(), open_deadline });
    Ok(())
}

// ---------------------------------------------------------------- buy_in

#[derive(Accounts)]
pub struct BuyIn<'info> {
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump, has_one = authority @ ZolError::BadAppAuthority)]
    pub app: Box<Account<'info, App>>,
    /// Nobody enters a pot except through the app.
    pub authority: Signer<'info>,
    pub session_key: Signer<'info>,
    #[account(mut, seeds = [seeds::BALANCE, app.key().as_ref(), session_key.key().as_ref()], bump = balance.bump)]
    pub balance: Box<Account<'info, Balance>>,
    #[account(mut, seeds = [seeds::POT, app.key().as_ref(), pot.pot_id.as_ref()], bump = pot.bump)]
    pub pot: Box<Account<'info, Pot>>,
    #[account(mut, seeds = [seeds::RECEIPT, pot.key().as_ref()], bump = receipt.bump)]
    pub receipt: Box<Account<'info, PotReceipt>>,
}

pub fn handle_buy_in(ctx: Context<BuyIn>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let app = &mut ctx.accounts.app;
    app.refresh(now);
    app.require_active(&ctx.accounts.config)?;
    let r = &mut ctx.accounts.receipt;
    require!(r.state == PotState::Open, ZolError::BadPotState);
    require!(now < r.open_deadline, ZolError::OpenDeadlinePassed);
    require!(amount > 0, ZolError::ZeroAmount);
    let b = &mut ctx.accounts.balance;
    require!(!b.exiting, ZolError::BalanceExiting);
    b.amount = b.amount.checked_sub(amount).ok_or(ZolError::InsufficientBalance)?;

    let player = ctx.accounts.session_key.key();
    let seat = match r.seat_of(&player) {
        Some(i) => i,
        None => {
            let i = r.seat_count as usize;
            require!(i < ctx.accounts.config.params.max_seats as usize, ZolError::PotFull);
            r.players[i] = player;
            r.seat_count += 1;
            i
        }
    };
    r.buy_ins[seat] = r.buy_ins[seat].checked_add(amount).ok_or(ZolError::Overflow)?;
    r.total = r.total.checked_add(amount).ok_or(ZolError::Overflow)?;
    let pot = &mut ctx.accounts.pot;
    pot.seat_count = r.seat_count;
    pot.amounts[seat] = r.buy_ins[seat];
    emit!(BoughtIn { pot: pot.key(), player, seat: seat as u8, amount });
    Ok(())
}

// ---------------------------------------------------------------- leave_pot

#[derive(Accounts)]
pub struct LeavePot<'info> {
    pub session_key: Signer<'info>,
    #[account(mut, seeds = [seeds::BALANCE, receipt.app.as_ref(), session_key.key().as_ref()], bump = balance.bump)]
    pub balance: Box<Account<'info, Balance>>,
    #[account(mut, seeds = [seeds::POT, receipt.app.as_ref(), receipt.pot_id.as_ref()], bump = pot.bump)]
    pub pot: Box<Account<'info, Pot>>,
    #[account(mut, seeds = [seeds::RECEIPT, pot.key().as_ref()], bump = receipt.bump)]
    pub receipt: Box<Account<'info, PotReceipt>>,
}

/// While the pot is Open the player alone can take their buy-in back, so an app
/// cannot trap anyone in a pot that has not started. Allowed even when frozen.
pub fn handle_leave_pot(ctx: Context<LeavePot>) -> Result<()> {
    let r = &mut ctx.accounts.receipt;
    require!(r.state == PotState::Open, ZolError::BadPotState);
    let player = ctx.accounts.session_key.key();
    let seat = r.seat_of(&player).ok_or(ZolError::NoSeat)?;
    let amount = r.buy_ins[seat];
    let n = r.seat_count as usize;
    let pot = &mut ctx.accounts.pot;
    // Shift later seats down so pot and receipt stay aligned and ordered.
    for i in seat..n - 1 {
        r.players[i] = r.players[i + 1];
        r.buy_ins[i] = r.buy_ins[i + 1];
        pot.amounts[i] = pot.amounts[i + 1];
    }
    r.players[n - 1] = Pubkey::default();
    r.buy_ins[n - 1] = 0;
    pot.amounts[n - 1] = 0;
    r.seat_count -= 1;
    pot.seat_count = r.seat_count;
    r.total -= amount;
    let b = &mut ctx.accounts.balance;
    b.amount = b.amount.checked_add(amount).ok_or(ZolError::Overflow)?;
    emit!(LeftPot { pot: pot.key(), player, amount });
    Ok(())
}

// ---------------------------------------------------------------- delegate_pot

/// Loads a ledger-owned pot from raw account data, applies `f`, writes it back,
/// and returns its pot ID. Used where the account must stay untyped for delegation.
fn update_pot(info: &AccountInfo, f: impl FnOnce(&mut Pot) -> Result<()>) -> Result<[u8; 32]> {
    let mut data = info.try_borrow_mut_data()?;
    let mut pot = Pot::try_deserialize(&mut &data[..])?;
    f(&mut pot)?;
    let mut w: &mut [u8] = &mut data[..];
    pot.try_serialize(&mut w)?;
    Ok(pot.pot_id)
}

#[delegate]
#[derive(Accounts)]
pub struct DelegatePot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump, has_one = authority @ ZolError::BadAppAuthority)]
    pub app: Box<Account<'info, App>>,
    pub authority: Signer<'info>,
    #[account(mut, seeds = [seeds::RECEIPT, pot.key().as_ref()], bump = receipt.bump, constraint = receipt.app == app.key() @ ZolError::BadAccount)]
    pub receipt: Box<Account<'info, PotReceipt>>,
    /// CHECK: the pot PDA; checked against the receipt and loaded by hand because
    /// after delegation it belongs to the delegation program.
    #[account(mut, del, address = receipt.pot @ ZolError::BadAccount)]
    pub pot: UncheckedAccount<'info>,
}

pub fn handle_delegate_pot(
    ctx: Context<DelegatePot>,
    expected: Vec<SeatStake>,
    session_deadline: i64,
    validator: Pubkey,
) -> Result<()> {
    // Without an explicit validator the delegation program picks its default one,
    // and the pot never appears in the app's rollup (spike 0.9).
    require_keys_neq!(validator, Pubkey::default(), ZolError::BadParams);
    let now = Clock::get()?.unix_timestamp;
    let p = ctx.accounts.config.params;
    {
        let app = &mut ctx.accounts.app;
        app.refresh(now);
        app.require_active(&ctx.accounts.config)?;
    }
    let r = &mut ctx.accounts.receipt;
    require!(r.state == PotState::Open, ZolError::BadPotState);
    require!(now < r.open_deadline, ZolError::OpenDeadlinePassed);
    // Players can leave until the start, so the app must state who it expects.
    let n = r.seat_count as usize;
    require!(n > 0 && expected.len() == n, ZolError::ReceiptMismatch);
    for (i, s) in expected.iter().enumerate() {
        require!(r.players[i] == s.player && r.buy_ins[i] == s.amount, ZolError::ReceiptMismatch);
    }
    let min = now.checked_add(p.min_session_secs).ok_or(ZolError::Overflow)?;
    let max = now.checked_add(p.max_session_secs).ok_or(ZolError::Overflow)?;
    require!(session_deadline >= min && session_deadline <= max, ZolError::BadDeadline);
    r.state = PotState::Delegated;
    r.session_deadline = session_deadline;
    r.validator = validator;
    let total = r.total;

    // Mark the pot delegated, write it back, then hand it to the delegation program.
    let pot_info = ctx.accounts.pot.to_account_info();
    require_keys_eq!(*pot_info.owner, crate::ID, ZolError::BadPotState);
    let pot_id = update_pot(&pot_info, |pot| {
        require!(pot.seat_count as usize == n, ZolError::PotInvalid);
        pot.delegated = true;
        Ok(())
    })?;
    let app_key = ctx.accounts.app.key();
    ctx.accounts.delegate_pot(
        &ctx.accounts.payer,
        &[seeds::POT, app_key.as_ref(), pot_id.as_ref()],
        DelegateConfig { validator: Some(validator), ..Default::default() },
    )?;
    emit!(PotDelegated { pot: ctx.accounts.pot.key(), session_deadline, total });
    Ok(())
}

// ---------------------------------------------------------------- move_seat (rollup)

#[derive(Accounts)]
pub struct MoveSeat<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [seeds::POT, pot.app.as_ref(), pot.pot_id.as_ref()], bump = pot.bump, has_one = app_authority @ ZolError::BadAppAuthority)]
    pub pot: Box<Account<'info, Pot>>,
    /// CHECK: compared to the signer
    #[account(address = authority.key() @ ZolError::BadAppAuthority)]
    pub app_authority: UncheckedAccount<'info>,
}

/// Moves value between two seats of one delegated pot. It cannot create value,
/// add a player or touch another pot.
pub fn handle_move_seat(ctx: Context<MoveSeat>, from: u8, to: u8, amount: u64) -> Result<()> {
    let pot = &mut ctx.accounts.pot;
    require!(pot.delegated, ZolError::BadPotState);
    let n = pot.seat_count;
    require!(from < n && to < n && from != to, ZolError::BadSeat);
    let (f, t) = (from as usize, to as usize);
    pot.amounts[f] = pot.amounts[f].checked_sub(amount).ok_or(ZolError::InsufficientBalance)?;
    pot.amounts[t] = pot.amounts[t].checked_add(amount).ok_or(ZolError::Overflow)?;
    Ok(())
}

// ---------------------------------------------------------------- close_pot (rollup)

#[commit]
#[derive(Accounts)]
pub struct ClosePot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub authority: Signer<'info>,
    /// CHECK: loaded by hand; must be written before the commit CPI.
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: MagicBlock's fee vault for the validator; the magic program checks it. Needed
    /// when the payer is itself a delegated account (an app's PDA in a scheduled task).
    #[account(mut)]
    pub magic_fee_vault: Option<UncheckedAccount<'info>>,
    /// The app's escrow authority for `action` (the app signs for its PDA).
    pub escrow_authority: Option<Signer<'info>>,
}

/// Commits the pot and returns it to Solana. With `action`, the delegation program then runs
/// that instruction on Solana right after the pot is back (paid from the app's escrow): an
/// app can pay its players out without a keeper. The action may only call the pot's app.
pub fn handle_close_pot(ctx: Context<ClosePot>, action: Option<PotAction>) -> Result<()> {
    let pot_info = ctx.accounts.pot.to_account_info();
    require_keys_eq!(*pot_info.owner, crate::ID, ZolError::BadAccount);
    let authority = ctx.accounts.authority.key();
    update_pot(&pot_info, |pot| {
        require_keys_eq!(pot.app_authority, authority, ZolError::BadAppAuthority);
        let (expected, _) = Pubkey::find_program_address(&[seeds::POT, pot.app.as_ref(), pot.pot_id.as_ref()], &crate::ID);
        require_keys_eq!(expected, pot_info.key(), ZolError::BadAccount);
        require!(pot.delegated, ZolError::BadPotState);
        pot.delegated = false;
        Ok(())
    })?;
    let mut bundle = MagicIntentBundleBuilder::new(
        ctx.accounts.payer.to_account_info(),
        ctx.accounts.magic_context.to_account_info(),
        ctx.accounts.magic_program.to_account_info(),
    );
    if let Some(vault) = &ctx.accounts.magic_fee_vault {
        bundle = bundle.magic_fee_vault(vault.to_account_info());
    }
    let bundle = bundle.commit_and_undelegate(&[pot_info]);
    let Some(a) = action else {
        bundle.build_and_invoke()?;
        return Ok(());
    };
    // Only the pot's own app: its signing address is the pot's authority.
    let (app_authority, _) = Pubkey::find_program_address(&[seeds::APP_AUTHORITY], &a.program);
    require_keys_eq!(app_authority, authority, ZolError::BadAction);
    require!(
        a.accounts.len() <= limits::MAX_ACTION_ACCOUNTS
            && a.data.len() <= limits::MAX_ACTION_DATA
            && a.compute_units > 0
            && a.compute_units <= limits::MAX_ACTION_COMPUTE_UNITS,
        ZolError::BadAction
    );
    let escrow = ctx.accounts.escrow_authority.as_ref().ok_or(ZolError::BadAction)?;
    require!(escrow.is_signer, ZolError::BadAction);
    bundle
        .add_post_undelegate_actions([CallHandler {
            destination_program: a.program,
            accounts: a
                .accounts
                .iter()
                .map(|x| ShortAccountMeta { pubkey: x.pubkey.to_bytes().into(), is_writable: x.is_writable })
                .collect(),
            args: ActionArgs::new(a.data),
            escrow_authority: escrow.to_account_info(),
            compute_units: a.compute_units,
        }])
        .build_and_invoke()?;
    Ok(())
}

// ---------------------------------------------------------------- cash_out / refund_pot

#[derive(Accounts)]
pub struct PotOutcome<'info> {
    #[account(seeds = [seeds::CONFIG], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [seeds::APP, app.program_id.as_ref()], bump = app.bump)]
    pub app: Box<Account<'info, App>>,
    #[account(mut, seeds = [seeds::RECEIPT, pot.key().as_ref()], bump = receipt.bump, constraint = receipt.app == app.key() @ ZolError::BadAccount)]
    pub receipt: Box<Account<'info, PotReceipt>>,
    /// CHECK: address derived from the receipt's app and pot ID; owner and contents
    /// checked by hand because it may still belong to the delegation program.
    #[account(seeds = [seeds::POT, app.key().as_ref(), receipt.pot_id.as_ref()], bump)]
    pub pot: UncheckedAccount<'info>,
}

/// The pot's seat amounts if it has validly returned: owned by the ledger again,
/// same number of seats as players, and summing to the receipt total.
fn returned_amounts(pot: &AccountInfo, r: &PotReceipt) -> Option<[u64; limits::MAX_SEATS]> {
    if pot.owner != &crate::ID {
        return None;
    }
    let data = pot.try_borrow_data().ok()?;
    let p = Pot::try_deserialize(&mut &data[..]).ok()?;
    if p.seat_count != r.seat_count || p.app != r.app || p.pot_id != r.pot_id {
        return None;
    }
    let n = r.seat_count as usize;
    let sum: u128 = p.amounts[..n].iter().map(|a| *a as u128).sum();
    if sum != r.total as u128 || p.amounts[n..].iter().any(|a| *a != 0) {
        return None;
    }
    Some(p.amounts)
}

/// Credits `amounts[i]` to `receipt.players[i]`; balances come in receipt order
/// as remaining accounts.
fn pay_players<'info>(
    app: &Pubkey,
    r: &PotReceipt,
    amounts: &[u64; limits::MAX_SEATS],
    balances: &'info [AccountInfo<'info>],
    min_exit: Option<u64>,
) -> Result<()> {
    let n = r.seat_count as usize;
    require!(balances.len() == n, ZolError::BadAccount);
    for (i, info) in balances.iter().enumerate() {
        require!(!balances[..i].iter().any(|b| b.key == info.key), ZolError::DuplicateAccount);
        let mut b = Account::<Balance>::try_from(info)?;
        require!(b.app == *app && b.session_key == r.players[i], ZolError::BadAccount);
        b.amount = b.amount.checked_add(amounts[i]).ok_or(ZolError::Overflow)?;
        if let Some(min) = min_exit {
            if b.auto_exit && b.amount >= min {
                b.exiting = true;
            }
        }
        b.exit(&crate::ID)?;
    }
    Ok(())
}

/// Anyone. Allowed for frozen apps, so pots in the rollup can still come home.
pub fn handle_cash_out<'info>(ctx: Context<'info, PotOutcome<'info>>) -> Result<()> {
    let app = ctx.accounts.app.key();
    let min_exit = ctx.accounts.config.params.min_exit;
    let pot = &ctx.accounts.pot;
    let r = &mut ctx.accounts.receipt;
    require!(r.state == PotState::Delegated, ZolError::BadPotState);
    require!(pot.owner == &crate::ID, ZolError::PotNotReturned);
    let amounts = returned_amounts(pot, r).ok_or(ZolError::PotInvalid)?;
    pay_players(&app, r, &amounts, ctx.remaining_accounts, Some(min_exit))?;
    r.state = PotState::CashedOut;
    emit!(PotCashedOut { pot: pot.key(), total: r.total });
    Ok(())
}

/// Anyone. Returns every buy-in from the receipt when the pot never started in
/// time, or never validly returned after its session deadline plus grace.
pub fn handle_refund_pot<'info>(ctx: Context<'info, PotOutcome<'info>>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let grace = ctx.accounts.config.params.refund_grace_secs;
    let app = ctx.accounts.app.key();
    let pot = &ctx.accounts.pot;
    let r = &mut ctx.accounts.receipt;
    let allowed = match r.state {
        PotState::Open => now >= r.open_deadline,
        PotState::Delegated => {
            let deadline = r.session_deadline.checked_add(grace).ok_or(ZolError::Overflow)?;
            // A pot that returned and passes the checks can only be cashed out.
            now >= deadline && returned_amounts(pot, r).is_none()
        }
        _ => false,
    };
    require!(allowed, ZolError::RefundNotAllowed);
    let buy_ins = r.buy_ins;
    pay_players(&app, r, &buy_ins, ctx.remaining_accounts, None)?;
    r.state = PotState::Refunded;
    emit!(PotRefunded { pot: pot.key(), total: r.total });
    Ok(())
}
