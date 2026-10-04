//! Spike 0.7: rerandomized FROST over Pallas (ZIP 312), 2-of-3, trusted dealer and DKG.
//! The FROST group key is the Orchard `ak`; the spend-auth signature of the 0.3 payout is
//! produced by FROST using the PCZT's alpha as the randomizer.

mod common;

use common::*;
use ff::{Field, PrimeField};
use group::GroupEncoding;
use orchard::keys::Scope;
use pasta_curves::pallas;
use rand::RngCore;
use reddsa::frost::redpallas::{keys, SigningKey};
use zol_zcash_spikes::{
    checks,
    frost_vault::{self, fvk_from_parts, id, KeyPackages},
    inspect, payout,
    pool::Pool,
    verify,
};

fn random_view_parts() -> (pallas::Base, pallas::Scalar) {
    let mut b = [0u8; 64];
    rand::rngs::OsRng.fill_bytes(&mut b);
    let nk = frost_vault::to_base(&b);
    rand::rngs::OsRng.fill_bytes(&mut b);
    let rivk = frost_vault::to_scalar(&b);
    (nk, rivk)
}

#[test]
fn sign_rule_even_y() {
    let mut rng = rand::rngs::OsRng;
    // Orchard's ak encoding must have ỹ = 0; FullViewingKey::from_bytes rejects ỹ = 1.
    let (_, pubkeys) = frost_vault::dealer_2of3(&mut rng);
    let ak = frost_vault::group_key_bytes(&pubkeys);
    let (nk, rivk) = random_view_parts();
    assert!(fvk_from_parts(&ak, nk, rivk).is_some());
    let neg = (-Option::<pallas::Point>::from(pallas::Point::from_bytes(&ak)).unwrap()).to_bytes();
    assert_eq!(frost_vault::ytilde(&neg), 1);
    assert!(fvk_from_parts(&neg, nk, rivk).is_none(), "odd-ỹ ak rejected by orchard");

    // reddsa forces even ỹ after dealer keygen (post_generate), split, and DKG (post_dkg).
    let n = 32;
    let mut even = 0;
    for _ in 0..n {
        let (_, pk) = frost_vault::dealer_2of3(&mut rng);
        even += frost_vault::has_even_y(&pk) as u32;
    }
    println!("dealer: {even}/{n} group keys have ỹ = 0");
    assert_eq!(even, n);
    let mut odd_inputs = 0;
    for _ in 0..n {
        let sk = SigningKey::new(&mut rng);
        let vk = reddsa::frost::redpallas::VerifyingKey::from(&sk);
        let odd = frost_vault::ytilde(&vk.serialize().unwrap().try_into().unwrap()) == 1;
        odd_inputs += odd as u32;
        let (shares, pk) = keys::split(&sk, 3, 2, keys::IdentifierList::Default, &mut rng).unwrap();
        assert!(frost_vault::has_even_y(&pk));
        let expect = if odd { -vk.to_element() } else { vk.to_element() };
        assert_eq!(pk.verifying_key().to_element(), expect, "split negates an odd key");
        let _kp: keys::KeyPackage = shares.into_values().next().unwrap().try_into().unwrap();
    }
    println!("split: {odd_inputs}/{n} input keys had ỹ = 1 and were negated (key + all shares)");
    let mut even = 0;
    for _ in 0..8 {
        let (_, pk) = frost_vault::dkg_2of3(&mut rng);
        even += frost_vault::has_even_y(&pk) as u32;
    }
    println!("DKG: {even}/8 group keys have ỹ = 0");
    assert_eq!(even, 8);
}

/// Every 2-subset signs; 1 signer cannot.
#[test]
fn threshold_subsets() {
    let mut rng = rand::rngs::OsRng;
    let (kps, pubkeys) = frost_vault::dkg_2of3(&mut rng);
    let alpha = pallas::Scalar::random(&mut rng);
    let msg = [42u8; 32];
    let g = reddsa::frost::redpallas::VerifyingKey::new(pubkeys.verifying_key().to_element() + zol_zcash_spikes::perapp::generator() * alpha);
    for s in [[1, 2], [1, 3], [2, 3]] {
        let sig = frost_vault::sign(&kps, &pubkeys, &[id(s[0]), id(s[1])], alpha, &msg, &mut rng).unwrap();
        let rk = orchard::primitives::redpallas::VerificationKey::<orchard::primitives::redpallas::SpendAuth>::try_from(<[u8; 32]>::try_from(g.serialize().unwrap()).unwrap()).unwrap();
        rk.verify(&msg, &orchard::primitives::redpallas::Signature::from(sig)).unwrap();
    }
    let one = frost_vault::sign(&kps, &pubkeys, &[id(1)], alpha, &msg, &mut rng);
    println!("1-of-3 attempt: {one:?}");
    assert!(one.is_err());
}

fn frost_payout(pool: Pool, kps: &KeyPackages, pubkeys: &keys::PublicKeyPackage, label: &str) {
    println!("\n===== spike 0.7 :: {label} :: {pool:?} =====");
    let ak = frost_vault::group_key_bytes(pubkeys);
    assert_eq!(frost_vault::ytilde(&ak), 0);
    let (nk, rivk) = random_view_parts();
    let vault_fvk = fvk_from_parts(&ak, nk, rivk).expect("FVK from FROST ak");
    println!("ak (FROST group key) = {}", hex::encode(ak));
    println!("vault address (ext, d=0) raw = {}", hex::encode(vault_fvk.address_at(0u32, Scope::External).to_raw_address_bytes()));

    let fx = fixture(pool, vault_fvk.clone());
    let real = fx.built.meta.spend_action_index(0).unwrap();
    let pczt = payout::prove(payout::io_finalize(fx.built.pczt.clone()), pool);
    let sighash = payout::shielded_sighash(&pczt);
    let (alpha, rk) = inspect::with_bundle(&pczt, pool, |b| {
        let a = &b.actions()[real];
        checks::check_real_spend(a, &vault_fvk, &fx.head).unwrap();
        (a.spend().alpha().unwrap(), <[u8; 32]>::from(a.spend().rk()))
    });
    println!("alpha (from PCZT) = {}", hex::encode(alpha.to_repr()));
    println!("rk = {}", hex::encode(rk));

    // FROST round 1 + 2 with signers {1, 3}; randomizer = alpha.
    let sig = frost_vault::sign(kps, pubkeys, &[id(1), id(3)], alpha, &sighash, &mut rand::rngs::OsRng).expect("FROST sign");
    let pczt = payout::apply_signature(pczt, pool, real, sig).expect("Signer accepts FROST signature (verified under rk)");
    let tx = payout::extract(payout::finalize_spends(pczt), pool).expect("extract");
    let raw = verify::raw(&tx);
    let rep = verify::verify_raw(&raw, pool).expect("independent verification");
    println!("FROST-signed payout verifies: {rep:?}");
    zol_zcash_spikes::write_out(&format!("payout-frost-{label}-{pool:?}.hex"), &raw);
    let bundle = match pool {
        Pool::OrchardNu62 => tx.orchard_bundle(),
        Pool::IronwoodNu63 => tx.ironwood_bundle(),
    }
    .unwrap();
    assert_eq!(bundle.decrypt_outputs_with_keys(&[fx.payout_fvk.to_ivk(Scope::External)]).len(), 1);
    assert_eq!(bundle.decrypt_outputs_with_keys(&[vault_fvk.to_ivk(Scope::Internal)]).len(), 1);
}

#[test]
fn frost_dealer_payout() {
    let (kps, pubkeys) = frost_vault::dealer_2of3(&mut rand::rngs::OsRng);
    frost_payout(Pool::IronwoodNu63, &kps, &pubkeys, "dealer");
    frost_payout(Pool::OrchardNu62, &kps, &pubkeys, "dealer");
}

#[test]
fn frost_dkg_payout() {
    let (kps, pubkeys) = frost_vault::dkg_2of3(&mut rand::rngs::OsRng);
    frost_payout(Pool::IronwoodNu63, &kps, &pubkeys, "dkg");
    frost_payout(Pool::OrchardNu62, &kps, &pubkeys, "dkg");
}
