use anchor_lang::prelude::*;

#[event]
pub struct ConfigChanged {
    pub admin: Pubkey,
    pub attestor: Pubkey,
    pub paused: bool,
}

#[event]
pub struct AppRegistered {
    pub app: Pubkey,
    pub authority: Pubkey,
}

#[event]
pub struct AppActivating {
    pub app: Pubkey,
    pub vault_address: [u8; 43],
    pub genesis_head: [u8; 32],
    pub activates_at: i64,
}

#[event]
pub struct ActivationCancelled {
    pub app: Pubkey,
    pub guardian: Pubkey,
}

#[event]
pub struct AppFrozen {
    pub app: Pubkey,
    pub guardian: Pubkey,
}

#[event]
pub struct AppUnfrozen {
    pub app: Pubkey,
}

#[event]
pub struct DepositCredited {
    pub app: Pubkey,
    pub deposit_id: [u8; 32],
    pub index: u64,
    pub amount: u64,
    pub fee: u64,
    pub session_key: Pubkey,
}

#[event]
pub struct DepositVoided {
    pub app: Pubkey,
    pub deposit_id: [u8; 32],
}

#[event]
pub struct PotOpened {
    pub app: Pubkey,
    pub pot: Pubkey,
    pub open_deadline: i64,
}

#[event]
pub struct BoughtIn {
    pub pot: Pubkey,
    pub player: Pubkey,
    pub seat: u8,
    pub amount: u64,
}

#[event]
pub struct LeftPot {
    pub pot: Pubkey,
    pub player: Pubkey,
    pub amount: u64,
}

#[event]
pub struct PotDelegated {
    pub pot: Pubkey,
    pub session_deadline: i64,
    pub total: u64,
}

#[event]
pub struct PotCashedOut {
    pub pot: Pubkey,
    pub total: u64,
}

#[event]
pub struct PotRefunded {
    pub pot: Pubkey,
    pub total: u64,
}

#[event]
pub struct ExitRequested {
    pub app: Pubkey,
    pub session_key: Pubkey,
    pub amount: u64,
}

#[event]
pub struct Settled {
    pub app: Pubkey,
    pub sequence: u64,
    pub count: u8,
    pub total: u64,
}

#[event]
pub struct SettlementPaid {
    pub app: Pubkey,
    pub sequence: u64,
}
