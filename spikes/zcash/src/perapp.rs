//! Spike 0.8: per-app vault keys from one key generation.
//!
//! Spend side (public tweak, applied to every share):
//!   t_app(c)  = ToScalar(BLAKE2b-512("ZOL_app_ak_tweak", ak_group || len(app_id) || app_id || c))
//!   ak_app    = ak_group + [t_app] G_spendauth
//!   s_i,app   = s_i + t_app            (every share; Lagrange coefficients sum to 1)
//!   Y_i,app   = Y_i + [t_app] G
//! Viewing side (secret, per app, from a master viewing secret K):
//!   nk_app    = ToBase  (BLAKE2b-512("ZOL_app_view_key", K || 0x00 || len(app_id) || app_id || c))
//!   rivk_app  = ToScalar(BLAKE2b-512("ZOL_app_view_key", K || 0x01 || len(app_id) || app_id || c))
//! c is the smallest u32 such that ak_app != identity, ỹ(ak_app) = 0 and
//! FullViewingKey::from_bytes(ak_app || nk_app || rivk_app) succeeds.

use std::collections::BTreeMap;

use group::GroupEncoding;
use orchard::keys::FullViewingKey;
use pasta_curves::pallas;
use reddsa::frost::redpallas::{
    keys::{KeyPackage, PublicKeyPackage, SigningShare, VerifyingShare},
    Group, PallasGroup, VerifyingKey,
};

use crate::frost_vault::{fvk_from_parts, to_base, to_scalar, KeyPackages};

pub const TWEAK_PERSONAL: &[u8; 16] = b"ZOL_app_ak_tweak";
pub const VIEW_PERSONAL: &[u8; 16] = b"ZOL_app_view_key";

fn blake(personal: &[u8; 16], parts: &[&[u8]]) -> [u8; 64] {
    let mut st = blake2b_simd::Params::new().hash_length(64).personal(personal).to_state();
    for p in parts {
        st.update(p);
    }
    *st.finalize().as_array()
}

fn app_id_enc(app_id: &[u8]) -> Vec<u8> {
    let mut v = (app_id.len() as u16).to_le_bytes().to_vec();
    v.extend_from_slice(app_id);
    v
}

pub fn tweak(ak_group: &[u8; 32], app_id: &[u8], c: u32) -> pallas::Scalar {
    to_scalar(&blake(TWEAK_PERSONAL, &[ak_group, &app_id_enc(app_id), &c.to_le_bytes()]))
}

pub fn view_keys(master: &[u8; 32], app_id: &[u8], c: u32) -> (pallas::Base, pallas::Scalar) {
    let a = app_id_enc(app_id);
    let nk = to_base(&blake(VIEW_PERSONAL, &[master, &[0x00], &a, &c.to_le_bytes()]));
    let rivk = to_scalar(&blake(VIEW_PERSONAL, &[master, &[0x01], &a, &c.to_le_bytes()]));
    (nk, rivk)
}

pub fn generator() -> pallas::Point {
    <PallasGroup as Group>::generator()
}

#[derive(Clone, Debug)]
pub struct AppKeys {
    pub counter: u32,
    /// Number of counters rejected for ỹ = 1 before acceptance.
    pub rejected_odd_y: u32,
    pub t: pallas::Scalar,
    pub ak: [u8; 32],
    pub fvk: FullViewingKey,
}

/// Public part: anyone holding ak_group and the master viewing secret can run this.
pub fn derive_app(ak_group: &[u8; 32], master: &[u8; 32], app_id: &[u8]) -> AppKeys {
    let ak_g = Option::<pallas::Point>::from(pallas::Point::from_bytes(ak_group)).expect("ak_group");
    let mut rejected = 0;
    for c in 0u32.. {
        let t = tweak(ak_group, app_id, c);
        let ak_app = ak_g + generator() * t;
        let ak: [u8; 32] = ak_app.to_bytes();
        if bool::from(group::Group::is_identity(&ak_app)) {
            continue;
        }
        if ak[31] >> 7 == 1 {
            rejected += 1;
            continue;
        }
        let (nk, rivk) = view_keys(master, app_id, c);
        if let Some(fvk) = fvk_from_parts(&ak, nk, rivk) {
            return AppKeys { counter: c, rejected_odd_y: rejected, t, ak, fvk };
        }
    }
    unreachable!()
}

/// Shift every signer's share (and verifying share, and the group key) by t.
pub fn tweak_packages(kps: &KeyPackages, pubkeys: &PublicKeyPackage, t: pallas::Scalar) -> (KeyPackages, PublicKeyPackage) {
    let tg = generator() * t;
    let kps2 = kps
        .iter()
        .map(|(i, kp)| {
            (
                *i,
                KeyPackage::new(
                    *i,
                    SigningShare::new(kp.signing_share().to_scalar() + t),
                    VerifyingShare::new(kp.verifying_share().to_element() + tg),
                    VerifyingKey::new(kp.verifying_key().to_element() + tg),
                    *kp.min_signers(),
                ),
            )
        })
        .collect();
    let vs: BTreeMap<_, _> = pubkeys
        .verifying_shares()
        .iter()
        .map(|(i, v)| (*i, VerifyingShare::new(v.to_element() + tg)))
        .collect();
    let pk2 = PublicKeyPackage::new(vs, VerifyingKey::new(pubkeys.verifying_key().to_element() + tg), pubkeys.min_signers());
    (kps2, pk2)
}

/// Alternative sign-rule fix: negate the whole (tweaked) key and every share.
pub fn negate_packages(kps: &KeyPackages, pubkeys: &PublicKeyPackage) -> (KeyPackages, PublicKeyPackage) {
    let kps2 = kps
        .iter()
        .map(|(i, kp)| {
            (
                *i,
                KeyPackage::new(
                    *i,
                    SigningShare::new(-kp.signing_share().to_scalar()),
                    VerifyingShare::new(-kp.verifying_share().to_element()),
                    VerifyingKey::new(-kp.verifying_key().to_element()),
                    *kp.min_signers(),
                ),
            )
        })
        .collect();
    let vs: BTreeMap<_, _> = pubkeys
        .verifying_shares()
        .iter()
        .map(|(i, v)| (*i, VerifyingShare::new(-v.to_element())))
        .collect();
    let pk2 = PublicKeyPackage::new(vs, VerifyingKey::new(-pubkeys.verifying_key().to_element()), pubkeys.min_signers());
    (kps2, pk2)
}
