use anchor_lang::prelude::*;

/// Upper bounds fixed in the program. The admin can set parameters only within them.
pub mod limits {
    pub const MAX_GUARDIANS: usize = 7;
    pub const MAX_SEATS: usize = 16;
    pub const MAX_PAYOUTS: usize = zol_wire::MAX_PAYOUTS_PER_SETTLEMENT;
    /// 1% cap on each of the entry and exit fee.
    pub const MAX_FEE_BPS: u16 = 100;
    /// 0.001 ZEC cap on the per-payout network fee allowance.
    pub const MAX_NETWORK_FEE_ALLOWANCE: u64 = 100_000;
    /// Floor on the activation delay so guardians always get a window to cancel.
    pub const MIN_ACTIVATION_DELAY_SECS: i64 = 3_600;
    /// Floor on the refund grace. A normal return takes about a second, but a forced
    /// undelegation only completes after 9,000 slots (about 60 minutes; spike 0.9), so
    /// nothing shorter is guaranteed to outlast a result that is on its way back.
    pub const MIN_REFUND_GRACE_SECS: i64 = 3_900;
    pub const MAX_APP_DATA: usize = zol_wire::memo::APP_DATA_MAX;
}

pub mod seeds {
    pub const CONFIG: &[u8] = b"config";
    pub const APP: &[u8] = b"app";
    pub const DEPOSIT: &[u8] = b"deposit";
    pub const BALANCE: &[u8] = b"balance";
    pub const POT: &[u8] = b"pot";
    pub const RECEIPT: &[u8] = b"receipt";
    pub const SETTLEMENT: &[u8] = b"settlement";
    /// Seed an app program uses for the address that signs on its behalf.
    pub const APP_AUTHORITY: &[u8] = b"zol-app";
}

use limits::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub struct Params {
    pub entry_fee_bps: u16,
    pub exit_fee_bps: u16,
    pub network_fee_allowance: u64,
    pub min_exit: u64,
    pub max_seats: u8,
    pub max_open_secs: i64,
    pub min_session_secs: i64,
    pub max_session_secs: i64,
    /// Must exceed the longest commit-to-return time of MagicBlock.
    pub refund_grace_secs: i64,
    pub activation_delay_secs: i64,
    /// Lamports moved into the App account at registration.
    pub registration_bond: u64,
}

impl Params {
    pub fn validate(&self) -> Result<()> {
        use crate::error::ZolError;
        require!(self.entry_fee_bps <= MAX_FEE_BPS && self.exit_fee_bps <= MAX_FEE_BPS, ZolError::FeeTooHigh);
        require!(self.network_fee_allowance <= MAX_NETWORK_FEE_ALLOWANCE, ZolError::FeeTooHigh);
        require!(self.max_seats >= 2 && self.max_seats as usize <= MAX_SEATS, ZolError::BadParams);
        require!(self.max_open_secs > 0, ZolError::BadParams);
        require!(self.min_session_secs > 0 && self.min_session_secs <= self.max_session_secs, ZolError::BadParams);
        require!(self.refund_grace_secs >= MIN_REFUND_GRACE_SECS, ZolError::BadParams);
        require!(self.activation_delay_secs >= MIN_ACTIVATION_DELAY_SECS, ZolError::BadParams);
        // Every payout must stay above its own deductions.
        let net = self
            .min_exit
            .checked_sub(bps(self.min_exit, self.exit_fee_bps))
            .and_then(|v| v.checked_sub(self.network_fee_allowance))
            .ok_or(ZolError::BadParams)?;
        require!(net > 0, ZolError::BadParams);
        Ok(())
    }
}

/// Floor of `amount * bps / 10_000`.
pub fn bps(amount: u64, bps: u16) -> u64 {
    ((amount as u128 * bps as u128) / 10_000) as u64
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    /// FROST Ed25519 group key of the attestors.
    pub attestor: Pubkey,
    pub guardians: [Pubkey; MAX_GUARDIANS],
    pub guardian_count: u8,
    pub guardian_threshold: u8,
    /// Zcash network, as `zol_wire::Network`.
    pub network: u8,
    pub paused: bool,
    pub params: Params,
    pub bump: u8,
}

impl Config {
    pub fn is_guardian(&self, key: &Pubkey) -> bool {
        self.guardians[..self.guardian_count as usize].contains(key)
    }

    /// Counts distinct guardians among the signers in `accounts`.
    pub fn count_guardian_signers(&self, accounts: &[AccountInfo]) -> usize {
        let mut seen: [Option<Pubkey>; MAX_GUARDIANS] = [None; MAX_GUARDIANS];
        let mut n = 0;
        for a in accounts {
            if a.is_signer && self.is_guardian(a.key) && !seen.contains(&Some(*a.key)) {
                seen[n] = Some(*a.key);
                n += 1;
                if n == MAX_GUARDIANS {
                    break;
                }
            }
        }
        n
    }
}

pub fn validate_guardians(guardians: &[Pubkey], threshold: u8) -> Result<()> {
    use crate::error::ZolError;
    require!(!guardians.is_empty() && guardians.len() <= MAX_GUARDIANS, ZolError::BadGuardians);
    require!(threshold >= 1 && threshold as usize <= guardians.len(), ZolError::BadGuardians);
    for (i, g) in guardians.iter().enumerate() {
        require!(!guardians[i + 1..].contains(g) && *g != Pubkey::default(), ZolError::BadGuardians);
    }
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum AppState {
    /// Registered, no vault yet.
    Pending,
    /// Vault recorded; becomes Active at `activates_at` unless a guardian cancels.
    Activating,
    Active,
    Frozen,
}

#[account]
#[derive(InitSpace)]
pub struct App {
    pub program_id: Pubkey,
    /// The app program's address with seed `zol-app`; signs on the app's behalf.
    pub authority: Pubkey,
    pub registrant: Pubkey,
    pub state: AppState,
    pub activates_at: i64,
    pub vault_address: [u8; 43],
    /// Note commitment (cmx) of the vault's genesis head.
    pub genesis_head: [u8; 32],
    /// Credits are refused once liability plus unpaid would exceed this.
    pub deposit_cap: u64,
    /// sum(Balance.amount) + sum(live PotReceipt.total)
    pub liability: u64,
    pub deposit_count: u64,
    /// sum(Deposit.amount) over credited deposits
    pub credited_total: u64,
    pub fee_pool: u64,
    pub next_settlement: u64,
    pub paid_count: u64,
    pub unpaid_total: u64,
    pub bond: u64,
    pub bump: u8,
}

impl App {
    /// Applies a matured activation. Call at the start of any handler that reads `state`.
    pub fn refresh(&mut self, now: i64) {
        if self.state == AppState::Activating && now >= self.activates_at {
            self.state = AppState::Active;
        }
    }

    pub fn require_active(&self, config: &Config) -> Result<()> {
        require!(!config.paused, crate::error::ZolError::Paused);
        match self.state {
            AppState::Active => Ok(()),
            AppState::Frozen => err!(crate::error::ZolError::AppFrozen),
            _ => err!(crate::error::ZolError::AppNotActive),
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum DepositState {
    Credited,
    Voided,
}

#[account]
#[derive(InitSpace)]
pub struct Deposit {
    pub state: DepositState,
    pub app: Pubkey,
    /// Position among this app's credited deposits; meaningless when voided.
    pub index: u64,
    pub amount: u64,
    pub session_key: Pubkey,
    pub payout_commitment: [u8; 32],
    pub pot_id: [u8; 32],
    /// Digest the attestors signed (credit or void).
    pub digest: [u8; 32],
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Balance {
    pub app: Pubkey,
    pub session_key: Pubkey,
    pub amount: u64,
    pub payout_commitment: [u8; 32],
    pub exiting: bool,
    pub auto_exit: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Pot {
    pub app: Pubkey,
    /// Copy of `App.authority`, so rollup instructions need no undelegated account.
    pub app_authority: Pubkey,
    pub pot_id: [u8; 32],
    pub seat_count: u8,
    pub amounts: [u64; MAX_SEATS],
    /// Set at delegation; only then may seats move.
    pub delegated: bool,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum PotState {
    Open,
    Delegated,
    CashedOut,
    Refunded,
}

#[account]
#[derive(InitSpace)]
pub struct PotReceipt {
    pub app: Pubkey,
    pub pot: Pubkey,
    pub pot_id: [u8; 32],
    pub state: PotState,
    pub seat_count: u8,
    pub players: [Pubkey; MAX_SEATS],
    pub buy_ins: [u64; MAX_SEATS],
    pub total: u64,
    pub open_deadline: i64,
    pub session_deadline: i64,
    /// Rollup validator named at delegation; the delegation program accepts commits
    /// only from it, so it is trusted for this pot's outcome.
    pub validator: Pubkey,
    pub bump: u8,
}

impl PotReceipt {
    pub fn is_live(&self) -> bool {
        matches!(self.state, PotState::Open | PotState::Delegated)
    }

    pub fn seat_of(&self, player: &Pubkey) -> Option<usize> {
        self.players[..self.seat_count as usize].iter().position(|p| p == player)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct PayoutEntry {
    pub commitment: [u8; 32],
    pub amount: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum SettlementState {
    Unpaid,
    Paid,
}

#[account]
#[derive(InitSpace)]
pub struct Settlement {
    pub app: Pubkey,
    pub sequence: u64,
    pub state: SettlementState,
    pub count: u8,
    pub payouts: [PayoutEntry; MAX_PAYOUTS],
    pub total: u64,
    pub bump: u8,
}

/// One expected stake passed to `delegate_pot`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeatStake {
    pub player: Pubkey,
    pub amount: u64,
}
