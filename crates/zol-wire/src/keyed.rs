//! Keyed identifiers derived with gateway secrets (design v0.3, "Attestation and
//! credit"). Gateway-only: the keys never reach Solana.
//!
//! Both are BLAKE2b-256 in keyed mode (a PRF) with a 16-byte personalization.
//! The deposit-ID key is fixed for the life of an app so one note has one ID.

use crate::{tags, ORCHARD_ADDR_LEN};

/// 32-byte gateway secret.
pub type GatewayKey = [u8; 32];

fn prf(key: &GatewayKey, personal: &[u8; 16], parts: &[&[u8]]) -> [u8; 32] {
    let mut st = blake2b_simd::Params::new()
        .hash_length(32)
        .key(key)
        .personal(personal)
        .to_state();
    for p in parts {
        st.update(p);
    }
    st.finalize().as_bytes().try_into().unwrap()
}

/// Deposit ID: keyed hash of the note commitment (Orchard `cmx`).
pub fn deposit_id(key: &GatewayKey, note_commitment: &[u8; 32]) -> [u8; 32] {
    prf(key, tags::DEPOSIT_ID, &[note_commitment])
}

/// Payout commitment: keyed hash of the payout address, the session key and the app.
pub fn payout_commitment(
    key: &GatewayKey,
    payout_address: &[u8; ORCHARD_ADDR_LEN],
    session_key: &[u8; 32],
    app: &[u8; 32],
) -> [u8; 32] {
    prf(key, tags::PAYOUT_CMT, &[payout_address, session_key, app])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_and_separated() {
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];
        let cmx = [5u8; 32];
        assert_ne!(deposit_id(&k1, &cmx), deposit_id(&k2, &cmx));
        let a = [6u8; 43];
        let s = [7u8; 32];
        let app = [8u8; 32];
        let p = payout_commitment(&k1, &a, &s, &app);
        assert_ne!(p, payout_commitment(&k1, &a, &[9u8; 32], &app), "session key bound");
        assert_ne!(p, payout_commitment(&k1, &a, &s, &[9u8; 32]), "app bound");
        assert_ne!(p, payout_commitment(&k2, &a, &s, &app), "keyed");
    }
}
