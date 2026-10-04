//! Spike 0.3: one-input vault payout through the real pczt roles, single-key vault.
//!
//! Creator(build_from_parts of zcash_primitives Builder -> orchard Builder::build_for_pczt)
//! -> IoFinalizer -> Prover -> Signer -> SpendFinalizer -> TxExtractor, then an
//! independent offline verification of the raw transaction.

mod common;

use std::convert::Infallible;
use std::time::Instant;

use common::*;
use orchard::keys::Scope;
use zcash_primitives::transaction::{
    builder::{BuildConfig, Builder, BundlePadding},
    fees::zip317,
};
use zcash_protocol::{consensus::TEST_NETWORK, memo::MemoBytes, value::Zatoshis};
use zol_zcash_spikes::{checks, inspect, payout, pool::Pool, verify};

fn run(pool: Pool) {
    println!("\n===== spike 0.3 :: {pool:?} (branch {:?}, height {}) =====", pool.branch_id(), pool.target_height());
    let (ask, vault_fvk) = single_key_vault();
    let fx = fixture(pool, vault_fvk.clone());
    let meta = &fx.built.meta;
    println!("fee = {} zat, new head value = {} zat", fx.built.fee, fx.built.head_value);
    assert_eq!(fx.built.head_value, HEAD_VALUE - PAYOUT_VALUE - fx.built.fee);

    // --- Creator stage: shape of the bundle.
    let pczt = fx.built.pczt.clone();
    let real_idx = meta.spend_action_index(0).expect("real spend index");
    let n_actions = inspect::with_bundle(&pczt, pool, |b| b.actions().len());
    assert_eq!(n_actions, 2, "1 real spend + 2 outputs => 2 actions");
    let dummy_idx: Vec<usize> = (0..n_actions).filter(|i| *i != real_idx).collect();
    assert_eq!(dummy_idx.len(), 1, "exactly one dummy spend");
    println!("actions = {n_actions}; real spend at {real_idx}; dummy spend at {:?}", dummy_idx);
    println!("payout output at {:?}, head output at {:?}", meta.output_action_index(0), meta.output_action_index(1));
    println!("-- after Creator:\n{}", inspect::FieldPresence::HEADER);
    for p in inspect::presence(&pczt, pool) {
        println!("{}", p.row());
    }
    inspect::with_bundle(&pczt, pool, |b| {
        let d = &b.actions()[dummy_idx[0]];
        assert!(d.spend().dummy_sk().is_some(), "Creator output carries the dummy spending key");
        assert_eq!(d.spend().value().unwrap().inner(), 0);
        assert!(d.spend().spend_auth_sig().is_none());
        let r = &b.actions()[real_idx];
        assert!(r.spend().dummy_sk().is_none());
        assert!(checks::is_vault_spend(r, &vault_fvk));
        assert!(!checks::is_vault_spend(d, &vault_fvk));
        assert!(b.bsk().is_none());
    });
    let pczt_bytes = pczt.clone().serialize().unwrap();
    println!("serialized PCZT (Creator) = {} bytes", pczt_bytes.len());

    // --- IoFinalizer: signs dummy spends with dummy_sk, then drops dummy_sk; sets bsk.
    let pczt = payout::io_finalize(pczt);
    let sighash = payout::shielded_sighash(&pczt);
    println!("-- after IoFinalizer:\n{}", inspect::FieldPresence::HEADER);
    for p in inspect::presence(&pczt, pool) {
        println!("{}", p.row());
    }
    inspect::with_bundle(&pczt, pool, |b| {
        checks::check_no_dummy_sk(b).expect("dummy_sk cleared by IoFinalizer");
        assert!(b.bsk().is_some());
        let d = &b.actions()[dummy_idx[0]];
        checks::check_dummy_sig(d, &sighash).expect("dummy sig valid under rk");
    });

    // --- Prover.
    let t = Instant::now();
    let _ = pool.proving_key();
    println!("proving key build: {:?}", t.elapsed());
    let t = Instant::now();
    let pczt = payout::prove(pczt, pool);
    println!("proof creation (2 actions): {:?}", t.elapsed());

    // --- Signer (single key) for the real spend.
    let pczt = payout::sign_single(pczt, pool, real_idx, &ask);
    // --- SpendFinalizer (no-op for shielded-only, but part of the role sequence).
    let pczt = payout::finalize_spends(pczt);
    // --- TxExtractor (itself verifies proof + signatures via BatchValidator).
    let t = Instant::now();
    let tx = payout::extract(pczt, pool).expect("extract");
    println!("extract (+verify): {:?}", t.elapsed());

    // --- Independent offline verification from raw bytes.
    let raw = verify::raw(&tx);
    let rep = verify::verify_raw(&raw, pool).expect("independent verification");
    println!("independent verification OK: {rep:?}");
    assert_eq!(rep.sighash, sighash, "sighash recomputed from raw tx == pczt Signer's sighash");
    assert_eq!(rep.value_balance, fx.built.fee as i64, "value balance == fee");
    println!("txid = {} ; sighash = {} ; sighash==txid bytes? {}", rep.txid, hex32(&rep.sighash), {
        let mut t = rep.sighash;
        t.reverse();
        hex::encode(t) == rep.txid
    });
    zol_zcash_spikes::write_out(&format!("payout-{pool:?}.hex"), &raw);

    // Nullifier of the head is revealed; recipients decrypt their notes.
    let bundle = match pool {
        Pool::OrchardNu62 => tx.orchard_bundle(),
        Pool::IronwoodNu63 => tx.ironwood_bundle(),
    }
    .unwrap();
    let head_nf = fx.head.nullifier(&vault_fvk);
    assert!(bundle.actions().iter().any(|a| *a.nullifier() == head_nf), "head nullifier revealed");
    let got = bundle.decrypt_outputs_with_keys(&[fx.payout_fvk.to_ivk(Scope::External)]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].2.value().inner(), PAYOUT_VALUE);
    assert_eq!(got[0].4, fx.payout_memo);
    let got = bundle.decrypt_outputs_with_keys(&[vault_fvk.to_ivk(Scope::External), vault_fvk.to_ivk(Scope::Internal)]);
    assert_eq!(got.len(), 1, "exactly one output addressed to the vault (the new head)");
    assert_eq!(got[0].2.value().inner(), fx.built.head_value);
    assert_eq!(got[0].4, fx.head_memo);
    // OVK recovery: the vault can recover both outputs it sent.
    let rec = bundle.recover_outputs_with_ovks(&[vault_fvk.to_ovk(Scope::External), vault_fvk.to_ovk(Scope::Internal)]);
    println!("outputs recoverable with vault OVKs: {}", rec.len());
    assert_eq!(rec.len(), 2);
}

#[test]
fn payout_orchard_nu6_2_v5() {
    run(Pool::OrchardNu62);
}

#[test]
fn payout_ironwood_nu6_3_v6() {
    run(Pool::IronwoodNu63);
}

/// After NU6.3 the Orchard pool disables cross-address transfers: a vault holding Orchard
/// notes cannot pay an external address from the Orchard pool.
#[test]
fn orchard_pool_after_nu6_3_cannot_pay_external_address() {
    let (_ask, vault_fvk) = single_key_vault();
    // A V2 (Orchard-pool) note.
    let head = zol_zcash_spikes::notes::receive(Pool::OrchardNu62, &vault_fvk, HEAD_VALUE, memo(b"x"));
    let mut tree = zol_zcash_spikes::tree::Tree::new();
    let pos = tree.append(&head.commitment().into());
    let anchor = tree.root();
    let mut b = Builder::new(
        TEST_NETWORK,
        Pool::IronwoodNu63.target_height(),
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: Some(anchor),
            ironwood_anchor: None,
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    );
    let spend = b.add_orchard_spend::<Infallible>(vault_fvk.clone(), head, tree.path(pos));
    println!("add_orchard_spend at NU6.3: {:?}", spend.as_ref().map(|_| ()));
    let out = b.add_orchard_output::<Infallible>(None, external_address(), Zatoshis::from_u64(600_000).unwrap(), MemoBytes::empty());
    println!("add_orchard_output (external) at NU6.3: {:?}", out.as_ref().map(|_| ()));
    assert!(
        matches!(out, Err(zcash_primitives::transaction::builder::Error::OrchardRecipient(orchard::builder::OutputError::CrossAddressDisabled))),
        "Orchard pool after NU6.3 rejects an output to a foreign address"
    );
    // A spend-to-self (wallet change) is still accepted; it is the only Orchard-pool shape left.
    let chg = b.add_orchard_change_output::<Infallible>(
        vault_fvk.clone(),
        None,
        vault_fvk.address_at(0u32, Scope::Internal),
        Zatoshis::from_u64(HEAD_VALUE - 15_000).unwrap(),
        MemoBytes::empty(),
    );
    println!("add_orchard_change_output at NU6.3: {:?}", chg.as_ref().map(|_| ()));
    let fee = b.get_fee(&zip317::FeeRule::standard()).map(u64::from);
    println!("ZIP 317 fee for 1 spend + 1 self-change (cross-address disabled => spend and output never share an action): {fee:?}");
}
