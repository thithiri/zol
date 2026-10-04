//! Spike 0.8: per-app key tweak on top of one FROST key generation.

mod common;

use common::*;
use ff::{Field, PrimeField};
use group::GroupEncoding;
use orchard::{
    keys::Scope,
    primitives::redpallas::{Signature, SpendAuth, VerificationKey},
};
use pasta_curves::pallas;
use rand::RngCore;
use zol_zcash_spikes::{
    checks,
    frost_vault::{self, id},
    inspect, notes, payout,
    perapp::{self, generator},
    pool::Pool,
    verify,
};

fn rk_bytes(ak: &[u8; 32], alpha: pallas::Scalar) -> [u8; 32] {
    let p = Option::<pallas::Point>::from(pallas::Point::from_bytes(ak)).unwrap();
    (p + generator() * alpha).to_bytes()
}

fn verify_under(rk: [u8; 32], msg: &[u8], sig: [u8; 64]) -> bool {
    VerificationKey::<SpendAuth>::try_from(rk).unwrap().verify(msg, &Signature::from(sig)).is_ok()
}

#[test]
fn derivation_properties() {
    let mut rng = rand::rngs::OsRng;
    let (kps, pubkeys) = frost_vault::dkg_2of3(&mut rng);
    let ak_group = frost_vault::group_key_bytes(&pubkeys);
    let mut master = [0u8; 32];
    rng.fill_bytes(&mut master);

    // Counter statistics over many app ids.
    let mut hist = [0u32; 8];
    let n = 256;
    for _ in 0..n {
        let mut app = [0u8; 32];
        rng.fill_bytes(&mut app);
        let k = perapp::derive_app(&ak_group, &master, &app);
        assert_eq!(frost_vault::ytilde(&k.ak), 0);
        assert_eq!(k.rejected_odd_y, k.counter, "only the sign rule ever forced a retry");
        hist[(k.counter as usize).min(7)] += 1;
    }
    println!("accepted counter histogram over {n} app ids (c=0..6, >=7): {hist:?}");

    // Shares: every share shifts by t (Lagrange coefficients sum to 1).
    let app_a = [0xA1u8; 32];
    let a = perapp::derive_app(&ak_group, &master, &app_a);
    let (kps_a, pk_a) = perapp::tweak_packages(&kps, &pubkeys, a.t);
    assert_eq!(frost_vault::group_key_bytes(&pk_a), a.ak);
    for kp in kps_a.values() {
        assert_eq!(generator() * kp.signing_share().to_scalar(), kp.verifying_share().to_element());
        assert_eq!(pk_a.verifying_shares()[kp.identifier()], *kp.verifying_share());
    }
    let msg = [9u8; 32];
    for s in [[1, 2], [1, 3], [2, 3]] {
        let alpha = pallas::Scalar::random(&mut rng);
        let sig = frost_vault::sign(&kps_a, &pk_a, &[id(s[0]), id(s[1])], alpha, &msg, &mut rng).unwrap();
        assert!(verify_under(rk_bytes(&a.ak, alpha), &msg, sig));
    }
    println!("app A: counter={} ak_app={} ; all 2-of-3 subsets sign under ak_app + [alpha]G", a.counter, hex::encode(a.ak));

    // Wrong variant: adding t/n (or t only to some shares) does NOT give ak_group + tG.
    let n_inv = pallas::Scalar::from(3u64).invert().unwrap();
    let (kps_bad, _) = perapp::tweak_packages(&kps, &pubkeys, a.t * n_inv);
    let alpha = pallas::Scalar::random(&mut rng);
    let bad = frost_vault::sign(&kps_bad, &pk_a, &[id(1), id(2)], alpha, &msg, &mut rng);
    println!("shares shifted by t/3 with group key ak_group+tG: {:?}", bad.as_ref().map(|_| ()));
    assert!(bad.is_err());

    // Sign-rule alternative: negate the whole tweaked key and every share.
    let (raw_t, raw_ak) = (0u32..)
        .map(|c| {
            let t = perapp::tweak(&ak_group, &app_a, c);
            let p = Option::<pallas::Point>::from(pallas::Point::from_bytes(&ak_group)).unwrap() + generator() * t;
            (t, p.to_bytes())
        })
        .find(|(_, b)| frost_vault::ytilde(b) == 1)
        .unwrap();
    let (kps_t, pk_t) = perapp::tweak_packages(&kps, &pubkeys, raw_t);
    let (kps_n, pk_n) = perapp::negate_packages(&kps_t, &pk_t);
    let ak_n = frost_vault::group_key_bytes(&pk_n);
    assert_eq!(frost_vault::ytilde(&ak_n), 0);
    assert_ne!(ak_n, raw_ak);
    let alpha = pallas::Scalar::random(&mut rng);
    let sig = frost_vault::sign(&kps_n, &pk_n, &[id(2), id(3)], alpha, &msg, &mut rng).unwrap();
    assert!(verify_under(rk_bytes(&ak_n, alpha), &msg, sig));
    println!("negation alternative: odd-ỹ ak_group+tG negated -> even; 2-of-3 shares -(s_i+t) sign correctly");
}

#[test]
fn per_app_payout_and_isolation() {
    let pool = Pool::IronwoodNu63;
    let mut rng = rand::rngs::OsRng;
    let (kps, pubkeys) = frost_vault::dkg_2of3(&mut rng);
    let ak_group = frost_vault::group_key_bytes(&pubkeys);
    let mut master = [0u8; 32];
    rng.fill_bytes(&mut master);
    let (app_a, app_b) = ([0xA1u8; 32], [0xB2u8; 32]);
    let a = perapp::derive_app(&ak_group, &master, &app_a);
    let b = perapp::derive_app(&ak_group, &master, &app_b);
    let (kps_a, pk_a) = perapp::tweak_packages(&kps, &pubkeys, a.t);
    let (kps_b, pk_b) = perapp::tweak_packages(&kps, &pubkeys, b.t);
    println!("app A: c={} ak={}\napp B: c={} ak={}", a.counter, hex::encode(a.ak), b.counter, hex::encode(b.ak));

    // --- Isolation of viewing keys.
    for scope in [Scope::External, Scope::Internal] {
        let to_b = b.fvk.address_at(0u32, scope);
        let dep = notes::make_deposit(pool, to_b, 50_000, memo(b"to app B"));
        assert!(dep.try_decrypt(&b.fvk.to_ivk(scope)).is_some());
        assert!(dep.try_decrypt(&a.fvk.to_ivk(Scope::External)).is_none());
        assert!(dep.try_decrypt(&a.fvk.to_ivk(Scope::Internal)).is_none());
        assert!(a.fvk.scope_for_address(&to_b).is_none());
        println!("deposit to app B ({scope:?}): B decrypts; A's external+internal ivk cannot; A.scope_for_address = None");
    }
    assert_ne!(a.fvk.to_bytes()[32..64], b.fvk.to_bytes()[32..64], "nk differs");
    assert_ne!(a.fvk.to_ovk(Scope::External).as_ref(), b.fvk.to_ovk(Scope::External).as_ref());

    // --- Full payout for app A signed by 2-of-3 FROST under the tweaked key.
    let fx = fixture(pool, a.fvk.clone());
    let real = fx.built.meta.spend_action_index(0).unwrap();
    let pczt = payout::prove(payout::io_finalize(fx.built.pczt.clone()), pool);
    let sighash = payout::shielded_sighash(&pczt);
    let alpha = inspect::with_bundle(&pczt, pool, |bb| {
        checks::check_real_spend(&bb.actions()[real], &a.fvk, &fx.head).unwrap();
        bb.actions()[real].spend().alpha().unwrap()
    });
    let sig = frost_vault::sign(&kps_a, &pk_a, &[id(2), id(3)], alpha, &sighash, &mut rng).unwrap();
    let tx = payout::extract(payout::finalize_spends(payout::apply_signature(pczt, pool, real, sig).unwrap()), pool).unwrap();
    let rep = verify::verify_raw(&verify::raw(&tx), pool).unwrap();
    println!("app A payout signed with tweaked 2-of-3 FROST shares verifies: txid {}", rep.txid);
    // App B's key packages cannot produce it (sanity).
    let sig_b = frost_vault::sign(&kps_b, &pk_b, &[id(2), id(3)], alpha, &sighash, &mut rng).unwrap();
    assert!(!verify_under(rk_bytes(&a.ak, alpha), &sighash, sig_b));

    // --- Cross-app equivalence: the tweak does not isolate SIGNING. A coordinator that
    // relabels app B's spend as app A's with alpha' = alpha_B + t_B - t_A gets app A's
    // shares to authorize app B's note. Only the note/nullifier check stops it.
    let fx_b = fixture(pool, b.fvk.clone());
    let real_b = fx_b.built.meta.spend_action_index(0).unwrap();
    let pczt_b = payout::prove(payout::io_finalize(fx_b.built.pczt.clone()), pool);
    let sighash_b = payout::shielded_sighash(&pczt_b);
    let alpha_b = inspect::with_bundle(&pczt_b, pool, |bb| bb.actions()[real_b].spend().alpha().unwrap());
    let alpha_rel = alpha_b + b.t - a.t;
    // Replace the alpha field in the (already proven) PCZT; alpha is not an effect.
    let mut bytes = pczt_b.clone().serialize().unwrap();
    let ab = alpha_b.to_repr();
    let at: Vec<usize> = bytes.windows(32).enumerate().filter(|(_, w)| *w == ab).map(|(i, _)| i).collect();
    assert_eq!(at.len(), 1);
    bytes[at[0]..at[0] + 32].copy_from_slice(&alpha_rel.to_repr());
    let relabeled = pczt::Pczt::parse(&bytes).unwrap();
    inspect::with_bundle(&relabeled, pool, |bb| {
        let act = &bb.actions()[real_b];
        assert!(checks::is_vault_spend(act, &a.fvk), "rk == ak_A + [alpha']G: the rk test alone says 'app A spend'");
        let e = checks::check_real_spend(act, &a.fvk, &fx.head).unwrap_err();
        println!("gateway for app A: rk test passes with alpha' but note check -> Err({e})");
    });
    let sig_rel = frost_vault::sign(&kps_a, &pk_a, &[id(1), id(2)], alpha_rel, &sighash_b, &mut rng).unwrap();
    let tx_rel = payout::extract(payout::finalize_spends(payout::apply_signature(relabeled, pool, real_b, sig_rel).unwrap()), pool)
        .expect("app A's shares authorized app B's note");
    let rep = verify::verify_raw(&verify::raw(&tx_rel), pool).unwrap();
    println!("UNSAFE if unchecked: app B's head spent with app A's tweaked shares (alpha' relabel) verifies: txid {}", rep.txid);
}

/// Deterministic vectors for the spec (docs/spikes/0.8-per-app-key-derivation.md).
#[test]
fn test_vectors() {
    // ak_group = [s]G for a fixed s, negated if needed so that ỹ = 0 (as reddsa does).
    let s = pallas::Scalar::from(0x5a5a_5a5a_u64);
    let mut p = generator() * s;
    if frost_vault::ytilde(&p.to_bytes()) == 1 {
        p = -p;
    }
    let ak_group = p.to_bytes();
    let master = [0x11u8; 32];
    println!("ak_group = {}", hex::encode(ak_group));
    println!("K        = {}", hex::encode(master));
    // Also include the first app id of the form [i; 32] that needs a counter retry.
    let retry = (1u8..=255)
        .map(|i| [i; 32])
        .find(|a| perapp::derive_app(&ak_group, &master, a).counter > 0)
        .unwrap();
    for app in [[0xA1u8; 32], [0xB2u8; 32], retry] {
        let k = perapp::derive_app(&ak_group, &master, &app);
        let (nk, rivk) = perapp::view_keys(&master, &app, k.counter);
        println!("app_id   = {}", hex::encode(app));
        println!("  c = {} (rejected for odd ỹ: {})", k.counter, k.rejected_odd_y);
        for c in 0..=k.counter {
            let t = perapp::tweak(&ak_group, &app, c);
            let q = (p + generator() * t).to_bytes();
            println!("  c={c}: t = {} ; ak_group+[t]G = {} (ỹ={})", hex::encode(t.to_repr()), hex::encode(q), frost_vault::ytilde(&q));
        }
        println!("  ak_app   = {}", hex::encode(k.ak));
        println!("  nk_app   = {}", hex::encode(nk.to_repr()));
        println!("  rivk_app = {}", hex::encode(rivk.to_repr()));
        println!("  addr(ext, j=0) raw = {}", hex::encode(k.fvk.address_at(0u32, Scope::External).to_raw_address_bytes()));
    }
}
