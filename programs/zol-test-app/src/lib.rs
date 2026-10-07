//! Test-only app. `forward` relays any instruction to the ledger with this
//! program's `zol-app` address as a signer, the way a real app's CPI would.
//! Tests use it to drive both honest and adversarial app behaviour.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;

declare_id!("Ha1wtAhZUABES7f97hq5sFLgrJva5TLggAmkfNmhdcG2");

pub const APP_AUTHORITY_SEED: &[u8] = b"zol-app";

#[program]
pub mod zol_test_app {
    use super::*;

    pub fn forward<'info>(ctx: Context<'info, Forward<'info>>, data: Vec<u8>) -> Result<()> {
        let authority = ctx.accounts.authority.key();
        let metas = ctx
            .remaining_accounts
            .iter()
            .map(|a| AccountMeta {
                pubkey: *a.key,
                is_signer: a.is_signer || *a.key == authority,
                is_writable: a.is_writable,
            })
            .collect();
        let ix = Instruction { program_id: ctx.accounts.ledger.key(), accounts: metas, data };
        let mut infos = ctx.remaining_accounts.to_vec();
        infos.push(ctx.accounts.ledger.to_account_info());
        invoke_signed(&ix, &infos, &[&[APP_AUTHORITY_SEED, &[ctx.bumps.authority]]])?;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Forward<'info> {
    /// CHECK: this program's signing address
    #[account(seeds = [APP_AUTHORITY_SEED], bump)]
    pub authority: UncheckedAccount<'info>,
    /// CHECK: the ledger program
    pub ledger: UncheckedAccount<'info>,
}
