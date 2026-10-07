use anchor_lang::prelude::*;

#[error_code]
pub enum ZolError {
    #[msg("Fee rate or allowance above the program's constant cap")]
    FeeTooHigh,
    #[msg("Parameters out of range")]
    BadParams,
    #[msg("Guardian set or threshold invalid")]
    BadGuardians,
    #[msg("Not enough guardian signatures")]
    GuardianThreshold,
    #[msg("Signer is not a guardian")]
    NotGuardian,
    #[msg("Only the program upgrade authority can initialize")]
    NotUpgradeAuthority,
    #[msg("Ledger is paused")]
    Paused,
    #[msg("App is frozen")]
    AppFrozen,
    #[msg("App is not active")]
    AppNotActive,
    #[msg("App is in the wrong state for this instruction")]
    BadAppState,
    #[msg("Account is not an executable program")]
    NotAProgram,
    #[msg("App authority is not the app program's zol-app address")]
    BadAppAuthority,
    #[msg("Previous instruction is not a matching Ed25519 attestation")]
    BadAttestation,
    #[msg("Deposit would exceed the app's cap")]
    OverCap,
    #[msg("Payout commitment differs from this balance's")]
    PayoutCommitmentMismatch,
    #[msg("Amount must be positive")]
    ZeroAmount,
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("Insufficient balance")]
    InsufficientBalance,
    #[msg("Balance is marked for exit")]
    BalanceExiting,
    #[msg("Pot is in the wrong state")]
    BadPotState,
    #[msg("Deadline out of range")]
    BadDeadline,
    #[msg("Open deadline has passed")]
    OpenDeadlinePassed,
    #[msg("Pot is full")]
    PotFull,
    #[msg("Player has no seat in this pot")]
    NoSeat,
    #[msg("Expected players and buy-ins differ from the receipt")]
    ReceiptMismatch,
    #[msg("Pot has not returned from the rollup")]
    PotNotReturned,
    #[msg("Pot does not match its receipt")]
    PotInvalid,
    #[msg("Seat index out of range")]
    BadSeat,
    #[msg("Refund conditions not met")]
    RefundNotAllowed,
    #[msg("Wrong account passed")]
    BadAccount,
    #[msg("Duplicate account")]
    DuplicateAccount,
    #[msg("Balance below the minimum exit")]
    BelowMinExit,
    #[msg("Balance is not marked for exit")]
    NotExiting,
    #[msg("Nothing to settle, or too many payouts")]
    BadPayoutCount,
    #[msg("Settlement is not the next unpaid one")]
    OutOfOrder,
    #[msg("Settlement already paid")]
    AlreadyPaid,
    #[msg("App data too long")]
    AppDataTooLong,
    #[msg("Unknown deposit flags")]
    BadFlags,
}
