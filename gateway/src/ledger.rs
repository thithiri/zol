//! The gateway's view of the Solana ledger (plan 3.6). Signers act only on
//! finalized state, so implementations read at the finalized commitment.

use anchor_lang::prelude::Pubkey;
use zol_ledger::state::{App, Balance, Config, Deposit, Settlement};
use zol_ledger::CreditArgs;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("transaction failed: {0}")]
    Tx(String),
    #[error("rpc: {0}")]
    Rpc(String),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

pub trait Ledger {
    fn config(&self) -> Result<Config>;
    fn app(&self, app_program: &Pubkey) -> Result<Option<App>>;
    fn deposit(&self, deposit_id: &[u8; 32]) -> Result<Option<Deposit>>;
    /// Every `Deposit` of this app, credited or voided.
    fn deposits(&self, app_program: &Pubkey) -> Result<Vec<([u8; 32], Deposit)>>;
    fn balance(&self, app_program: &Pubkey, session_key: &Pubkey) -> Result<Option<Balance>>;
    fn settlements(&self, app_program: &Pubkey) -> Result<Vec<Settlement>>;
    /// Unix time of the finalized slot.
    fn now(&self) -> Result<i64>;

    fn submit_credit(&mut self, app_program: &Pubkey, args: CreditArgs, signature: [u8; 64]) -> Result<()>;
    fn submit_void(&mut self, app_program: &Pubkey, deposit_id: [u8; 32], signature: [u8; 64]) -> Result<()>;
    fn submit_paid(&mut self, app_program: &Pubkey, sequence: u64, signature: [u8; 64]) -> Result<()>;
    /// Signs `freeze_app` with a guardian's Solana key (32-byte Ed25519 seed).
    fn freeze(&mut self, guardian_seed: &[u8; 32], app_program: &Pubkey) -> Result<()>;
}

/// The derived ledger-account address of a deposit, for indexers.
pub fn deposit_address(deposit_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[zol_ledger::state::seeds::DEPOSIT, deposit_id], &zol_ledger::ID).0
}
