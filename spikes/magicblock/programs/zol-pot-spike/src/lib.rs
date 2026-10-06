//! ZOL spike 0.9: a minimal "pot" PDA delegated to a MagicBlock ephemeral rollup.
//!
//! Base layer: init_config, set_config, init_pot, delegate_pot, cash_out_check.
//! Rollup:     move_seat (also reads the NON-delegated Config), commit_pot, close_pot.
use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{commit, delegate, ephemeral};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::MagicIntentBundleBuilder;

declare_id!("8WPEbc1bHHHtDLnQvDPEDYXrYSKeR1ZekCkG4sqpvJ8U");

pub const POT_SEED: &[u8] = b"pot";
pub const CONFIG_SEED: &[u8] = b"config";
pub const SEATS: usize = 8;

#[ephemeral]
#[program]
pub mod zol_pot_spike {
    use super::*;

    /// Base layer. Singleton config that is never delegated.
    pub fn init_config(ctx: Context<InitConfig>, value: u64) -> Result<()> {
        ctx.accounts.config.value = value;
        Ok(())
    }

    /// Base layer. Change the config while a pot is delegated, to test staleness in the ER.
    pub fn set_config(ctx: Context<SetConfig>, value: u64) -> Result<()> {
        ctx.accounts.config.value = value;
        Ok(())
    }

    /// Base layer. Create a pot with initial seat amounts.
    pub fn init_pot(ctx: Context<InitPot>, pot_id: u64, amounts: Vec<u64>) -> Result<()> {
        require!(amounts.len() <= SEATS, SpikeError::TooManySeats);
        let pot = &mut ctx.accounts.pot;
        pot.pot_id = pot_id;
        pot.seats = amounts.len() as u8;
        for (i, a) in amounts.iter().enumerate() {
            pot.amounts[i] = *a;
        }
        Ok(())
    }

    /// Base layer. Hand the pot to the delegation program.
    /// Optional remaining_accounts[0] = ER validator identity to pin.
    pub fn delegate_pot(ctx: Context<DelegatePot>, pot_id: u64) -> Result<()> {
        let id = pot_id.to_le_bytes();
        ctx.accounts.delegate_pot(
            &ctx.accounts.payer,
            &[POT_SEED, &id],
            DelegateConfig {
                validator: ctx.remaining_accounts.first().map(|a| a.key()),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// Rollup. Move value between two seats; also records what the ER sees in Config.
    pub fn move_seat(ctx: Context<MoveSeat>, from: u8, to: u8, amount: u64) -> Result<()> {
        let pot = &mut ctx.accounts.pot;
        let (f, t) = (from as usize, to as usize);
        require!(f < pot.seats as usize && t < pot.seats as usize, SpikeError::BadSeat);
        pot.amounts[f] = pot.amounts[f].checked_sub(amount).ok_or(SpikeError::Insufficient)?;
        pot.amounts[t] = pot.amounts[t].checked_add(amount).ok_or(SpikeError::Overflow)?;
        pot.moves += 1;
        pot.last_config_seen = ctx.accounts.config.value;
        msg!(
            "move_seat ok; config.value seen in ER = {}; config owner = {}",
            ctx.accounts.config.value,
            ctx.accounts.config.to_account_info().owner
        );
        Ok(())
    }

    /// Rollup. Commit the pot state to the base layer, keep it delegated.
    pub fn commit_pot(ctx: Context<CommitPot>) -> Result<()> {
        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit(&[ctx.accounts.pot.to_account_info()])
        .build_and_invoke()?;
        Ok(())
    }

    /// Rollup. Commit the pot and return it to the base layer (close_pot in the design).
    pub fn close_pot(ctx: Context<CommitPot>) -> Result<()> {
        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[ctx.accounts.pot.to_account_info()])
        .build_and_invoke()?;
        Ok(())
    }

    /// Base layer. The cash_out precondition: the pot is owned by this program again
    /// and deserializes; logs owner and sum. Fails with NotReturned while delegated.
    pub fn cash_out_check(ctx: Context<CashOutCheck>, pot_id: u64) -> Result<()> {
        let info = &ctx.accounts.pot;
        let (expected, _) =
            Pubkey::find_program_address(&[POT_SEED, &pot_id.to_le_bytes()], &crate::ID);
        require_keys_eq!(info.key(), expected, SpikeError::BadSeat);
        msg!("pot owner = {}", info.owner);
        if info.owner != &crate::ID {
            msg!(
                "not returned; owner is delegation program: {}",
                info.owner == &ephemeral_rollups_sdk::id()
            );
            return err!(SpikeError::NotReturned);
        }
        let data = info.try_borrow_data()?;
        let pot = Pot::try_deserialize(&mut &data[..])?;
        let sum: u64 = pot.amounts[..pot.seats as usize].iter().sum();
        msg!("returned; seats={} sum={} moves={}", pot.seats, sum, pot.moves);
        Ok(())
    }

    /// Base layer. Owner-program request for undelegation (DLP ix 26). The payer must be
    /// the delegation rent payer recorded at delegate time. Starts a 9000-slot timeout.
    pub fn request_pot_undelegation(ctx: Context<RequestUndel>, pot_id: u64) -> Result<()> {
        let id = pot_id.to_le_bytes();
        let bump = ctx.bumps.pot;
        let a = &ctx.accounts;
        let ix = anchor_lang::solana_program::instruction::Instruction {
            program_id: ephemeral_rollups_sdk::id(),
            accounts: vec![
                AccountMeta::new(a.payer.key(), true),
                AccountMeta::new_readonly(a.pot.key(), true),
                AccountMeta::new_readonly(crate::ID, false),
                AccountMeta::new(a.undelegation_request.key(), false),
                AccountMeta::new_readonly(a.delegation_record.key(), false),
                AccountMeta::new(a.delegation_metadata.key(), false),
                AccountMeta::new_readonly(a.system_program.key(), false),
            ],
            data: 26u64.to_le_bytes().to_vec(),
        };
        anchor_lang::solana_program::program::invoke_signed(
            &ix,
            &[
                a.payer.to_account_info(),
                a.pot.to_account_info(),
                a.owner_program.to_account_info(),
                a.undelegation_request.to_account_info(),
                a.delegation_record.to_account_info(),
                a.delegation_metadata.to_account_info(),
                a.system_program.to_account_info(),
            ],
            &[&[POT_SEED, &id, &[bump]]],
        )?;
        Ok(())
    }

    /// Base layer. After the request timed out, take the pot back with its last
    /// finalized base-layer state (DLP ix 27). Saves and restores the data around the CPI.
    pub fn rollback_pot(ctx: Context<RollbackPot>, pot_id: u64) -> Result<()> {
        let id = pot_id.to_le_bytes();
        let bump = ctx.bumps.pot;
        let a = &ctx.accounts;
        let saved: Vec<u8> = a.pot.try_borrow_data()?.to_vec();
        let ix = anchor_lang::solana_program::instruction::Instruction {
            program_id: ephemeral_rollups_sdk::id(),
            accounts: vec![
                AccountMeta::new(a.pot.key(), true),
                AccountMeta::new_readonly(crate::ID, false),
                AccountMeta::new(a.undelegation_request.key(), false),
                AccountMeta::new(a.delegation_record.key(), false),
                AccountMeta::new(a.delegation_metadata.key(), false),
                AccountMeta::new(a.rent_payer.key(), false),
                AccountMeta::new(a.commit_state.key(), false),
                AccountMeta::new(a.commit_record.key(), false),
                AccountMeta::new(a.commit_reimbursement.key(), false),
            ],
            data: 27u64.to_le_bytes().to_vec(),
        };
        anchor_lang::solana_program::program::invoke_signed(
            &ix,
            &[
                a.pot.to_account_info(),
                a.owner_program.to_account_info(),
                a.undelegation_request.to_account_info(),
                a.delegation_record.to_account_info(),
                a.delegation_metadata.to_account_info(),
                a.rent_payer.to_account_info(),
                a.commit_state.to_account_info(),
                a.commit_record.to_account_info(),
                a.commit_reimbursement.to_account_info(),
            ],
            &[&[POT_SEED, &id, &[bump]]],
        )?;
        let info = a.pot.to_account_info();
        require_keys_eq!(*info.owner, crate::ID, SpikeError::NotReturned);
        info.resize(saved.len())?;
        info.try_borrow_mut_data()?.copy_from_slice(&saved);
        msg!("rolled back; restored {} bytes", saved.len());
        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(pot_id: u64)]
pub struct RequestUndel<'info> {
    /// Must equal the delegation metadata rent_payer.
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: delegated pot PDA (owned by DLP)
    #[account(seeds = [POT_SEED, &pot_id.to_le_bytes()], bump)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,
    /// CHECK: DLP PDA ["undelegation-request", pot]
    #[account(mut)]
    pub undelegation_request: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    pub delegation_record: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    #[account(mut)]
    pub delegation_metadata: UncheckedAccount<'info>,
    /// CHECK: DLP
    #[account(address = ephemeral_rollups_sdk::id())]
    pub delegation_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(pot_id: u64)]
pub struct RollbackPot<'info> {
    /// CHECK: delegated pot PDA (owned by DLP until the CPI returns)
    #[account(mut, seeds = [POT_SEED, &pot_id.to_le_bytes()], bump)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: this program
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    #[account(mut)]
    pub undelegation_request: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    #[account(mut)]
    pub delegation_metadata: UncheckedAccount<'info>,
    /// CHECK: refunded rent
    #[account(mut)]
    pub rent_payer: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    #[account(mut)]
    pub commit_state: UncheckedAccount<'info>,
    /// CHECK: DLP PDA
    #[account(mut)]
    pub commit_record: UncheckedAccount<'info>,
    /// CHECK: validator identity (only used if a commit is pending)
    #[account(mut)]
    pub commit_reimbursement: UncheckedAccount<'info>,
    /// CHECK: DLP
    #[account(address = ephemeral_rollups_sdk::id())]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(init, payer = payer, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetConfig<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    pub payer: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(pot_id: u64)]
pub struct InitPot<'info> {
    #[account(init, payer = payer, space = 8 + Pot::INIT_SPACE,
              seeds = [POT_SEED, &pot_id.to_le_bytes()], bump)]
    pub pot: Account<'info, Pot>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[delegate]
#[derive(Accounts)]
#[instruction(pot_id: u64)]
pub struct DelegatePot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the pot PDA; checked by seeds, must be untyped for delegation.
    #[account(mut, del, seeds = [POT_SEED, &pot_id.to_le_bytes()], bump)]
    pub pot: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct MoveSeat<'info> {
    #[account(mut, seeds = [POT_SEED, &pot.pot_id.to_le_bytes()], bump)]
    pub pot: Account<'info, Pot>,
    #[account(seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct CommitPot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [POT_SEED, &pot.pot_id.to_le_bytes()], bump)]
    pub pot: Account<'info, Pot>,
}

#[derive(Accounts)]
pub struct CashOutCheck<'info> {
    /// CHECK: owner checked in handler
    pub pot: UncheckedAccount<'info>,
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub value: u64,
}

#[account]
#[derive(InitSpace)]
pub struct Pot {
    pub pot_id: u64,
    pub seats: u8,
    pub amounts: [u64; SEATS],
    pub moves: u64,
    pub last_config_seen: u64,
}

#[error_code]
pub enum SpikeError {
    #[msg("too many seats")]
    TooManySeats,
    #[msg("bad seat")]
    BadSeat,
    #[msg("insufficient")]
    Insufficient,
    #[msg("overflow")]
    Overflow,
    #[msg("pot has not returned from the rollup")]
    NotReturned,
}
