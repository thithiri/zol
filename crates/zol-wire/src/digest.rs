//! Attestation and settlement digests (design v0.3, "Attestation and credit",
//! "The vault head").
//!
//! Every digest is SHA-256 over a domain tag followed by fixed-width fields in a
//! fixed order. The attestor (FROST Ed25519) signs the 32-byte digest; the ledger
//! recomputes it and checks the Ed25519 precompile instruction signed exactly it.

use crate::tags;

/// A 32-byte hash over the concatenation of `parts`.
///
/// The ledger implements this with the `sol_sha256` syscall; host code uses
/// [`Sha256`] (feature `sha2`). Both must be plain SHA-256.
pub trait Hasher {
    fn hashv(parts: &[&[u8]]) -> [u8; 32];
}

#[cfg(feature = "sha2")]
pub struct Sha256;

#[cfg(feature = "sha2")]
impl Hasher for Sha256 {
    fn hashv(parts: &[&[u8]]) -> [u8; 32] {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        for p in parts {
            h.update(p);
        }
        h.finalize().into()
    }
}

/// Fields of a credit attestation. The Zcash height is deliberately absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditAttestation<'a> {
    pub network: u8,
    /// The app's program ID.
    pub app: [u8; 32],
    pub deposit_id: [u8; 32],
    /// Full note value in zatoshi.
    pub amount: u64,
    pub session_key: [u8; 32],
    pub payout_commitment: [u8; 32],
    pub flags: u8,
    pub pot_id: [u8; 32],
    pub app_data: &'a [u8],
}

impl CreditAttestation<'_> {
    pub fn digest<H: Hasher>(&self) -> [u8; 32] {
        let len = (self.app_data.len() as u16).to_le_bytes();
        H::hashv(&[
            tags::CREDIT,
            &[self.network],
            &self.app,
            &self.deposit_id,
            &self.amount.to_le_bytes(),
            &self.session_key,
            &self.payout_commitment,
            &[self.flags],
            &self.pot_id,
            &len,
            self.app_data,
        ])
    }
}

pub fn void_digest<H: Hasher>(network: u8, app: &[u8; 32], deposit_id: &[u8; 32]) -> [u8; 32] {
    H::hashv(&[tags::VOID, &[network], app, deposit_id])
}

pub fn paid_digest<H: Hasher>(network: u8, app: &[u8; 32], sequence: u64) -> [u8; 32] {
    H::hashv(&[tags::PAID, &[network], app, &sequence.to_le_bytes()])
}

/// One payout in a settlement record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Payout {
    pub commitment: [u8; 32],
    pub amount: u64,
}

/// Digest of one settlement record: app, sequence number and its payouts in order.
pub fn record_digest<H: Hasher>(app: &[u8; 32], sequence: u64, payouts: &[Payout]) -> [u8; 32] {
    // Chained so no allocation is needed for up to 16 payouts.
    let mut acc = H::hashv(&[
        tags::RECORD,
        app,
        &sequence.to_le_bytes(),
        &(payouts.len() as u8).to_le_bytes(),
    ]);
    for p in payouts {
        acc = H::hashv(&[&acc, &p.commitment, &p.amount.to_le_bytes()]);
    }
    acc
}

/// Digest of the records one vault transaction pays, carried in the new head memo.
/// `records` are the record digests in sequence order; `first_sequence` is the first one paid.
pub fn records_digest<H: Hasher>(app: &[u8; 32], first_sequence: u64, records: &[[u8; 32]]) -> [u8; 32] {
    let mut acc = H::hashv(&[
        tags::RECORDS,
        app,
        &first_sequence.to_le_bytes(),
        &(records.len() as u32).to_le_bytes(),
    ]);
    for r in records {
        acc = H::hashv(&[&acc, r]);
    }
    acc
}

#[cfg(all(test, feature = "sha2"))]
mod tests {
    use super::*;

    fn credit() -> CreditAttestation<'static> {
        CreditAttestation {
            network: 1,
            app: [1; 32],
            deposit_id: [2; 32],
            amount: 1_000_000,
            session_key: [3; 32],
            payout_commitment: [4; 32],
            flags: 1,
            pot_id: [0; 32],
            app_data: b"abc",
        }
    }

    #[test]
    fn credit_digest_binds_every_field() {
        let base = credit().digest::<Sha256>();
        let variants: [CreditAttestation; 9] = [
            CreditAttestation { network: 0, ..credit() },
            CreditAttestation { app: [9; 32], ..credit() },
            CreditAttestation { deposit_id: [9; 32], ..credit() },
            CreditAttestation { amount: 999_999, ..credit() },
            CreditAttestation { session_key: [9; 32], ..credit() },
            CreditAttestation { payout_commitment: [9; 32], ..credit() },
            CreditAttestation { flags: 0, ..credit() },
            CreditAttestation { pot_id: [9; 32], ..credit() },
            CreditAttestation { app_data: b"abd", ..credit() },
        ];
        for v in variants {
            assert_ne!(v.digest::<Sha256>(), base);
        }
    }

    #[test]
    fn message_types_do_not_collide() {
        let app = [1u8; 32];
        let id = [2u8; 32];
        assert_ne!(void_digest::<Sha256>(1, &app, &id), paid_digest::<Sha256>(1, &app, 0));
        assert_ne!(void_digest::<Sha256>(1, &app, &id), void_digest::<Sha256>(0, &app, &id));
        assert_ne!(paid_digest::<Sha256>(1, &app, 0), paid_digest::<Sha256>(1, &app, 1));
    }

    #[test]
    fn record_digest_is_order_sensitive() {
        let a = Payout { commitment: [1; 32], amount: 5 };
        let b = Payout { commitment: [2; 32], amount: 6 };
        let app = [0u8; 32];
        assert_ne!(record_digest::<Sha256>(&app, 0, &[a, b]), record_digest::<Sha256>(&app, 0, &[b, a]));
        assert_ne!(record_digest::<Sha256>(&app, 0, &[a]), record_digest::<Sha256>(&app, 1, &[a]));
        let r0 = record_digest::<Sha256>(&app, 0, &[a]);
        let r1 = record_digest::<Sha256>(&app, 1, &[b]);
        assert_ne!(records_digest::<Sha256>(&app, 0, &[r0, r1]), records_digest::<Sha256>(&app, 0, &[r0]));
    }
}
