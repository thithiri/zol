//! ZOL wire formats, shared by the ledger program, the gateway and the web page.
//!
//! Everything here is byte-exact and versioned. See `docs/design-v0.3.md`:
//! "Deposit" (memo layout), "Attestation and credit" (digests), "The vault head"
//! (head memo) and "Reserve accounting" (top-up memo).
//!
//! The crate is `no_std` and allocation-free so the ledger can use it on-chain.
//! Hashing is abstracted behind [`Hasher`] so the ledger can use the SHA-256 syscall.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

pub mod digest;
pub mod head;
#[cfg(feature = "keyed")]
pub mod keyed;
pub mod memo;
pub mod receipt;
pub mod text;

pub use digest::{CreditAttestation, Hasher, Payout};
pub use head::{HeadMemo, TopUpMemo, TopUpPurpose, Transition};
pub use memo::{DepositMemo, MemoError, MemoKind};

/// Length of a Zcash memo field.
pub const MEMO_LEN: usize = 512;
/// Length of a raw Orchard-format receiver (diversifier 11 bytes + pk_d 32 bytes).
/// After NU6.3 the vault lives in the Ironwood pool, which uses the same receivers.
pub const ORCHARD_ADDR_LEN: usize = 43;
/// Maximum number of payouts in one settlement record.
pub const MAX_PAYOUTS_PER_SETTLEMENT: usize = 16;

/// Zcash network a message is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Network {
    Mainnet = 0,
    Testnet = 1,
    Regtest = 2,
}

impl Network {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Mainnet),
            1 => Some(Self::Testnet),
            2 => Some(Self::Regtest),
            _ => None,
        }
    }
}

/// Domain tags. Fixed 16 bytes, NUL padded, pairwise distinct, so a signature or
/// digest for one message type can never be read as another.
pub mod tags {
    pub const MEMO_SIG: &[u8; 16] = b"ZOL/memo-sig/v2\0";
    pub const CREDIT: &[u8; 16] = b"ZOL/credit/v1\0\0\0";
    pub const VOID: &[u8; 16] = b"ZOL/void/v1\0\0\0\0\0";
    pub const PAID: &[u8; 16] = b"ZOL/paid/v1\0\0\0\0\0";
    pub const RECORD: &[u8; 16] = b"ZOL/record/v1\0\0\0";
    pub const RECORDS: &[u8; 16] = b"ZOL/records/v1\0\0";
    pub const DEPOSIT_ID: &[u8; 16] = b"ZOL_DepositId_v1";
    pub const PAYOUT_CMT: &[u8; 16] = b"ZOL_PayoutCmt_v1";
}

#[cfg(test)]
mod tag_tests {
    use super::tags::*;

    #[test]
    fn tags_are_distinct() {
        let all = [MEMO_SIG, CREDIT, VOID, PAID, RECORD, RECORDS, DEPOSIT_ID, PAYOUT_CMT];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
