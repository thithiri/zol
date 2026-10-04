//! Spike 0.4: confirm a dummy spend is worth zero from the fields the PCZT exposes to a
//! Signer, and verify its attached signature. No proof needed.

mod common;

use common::*;
use pczt::roles::redactor::Redactor;
use zol_zcash_spikes::{checks, inspect, payout, pool::Pool};

fn run(pool: Pool) {
    println!("\n===== spike 0.4 :: {pool:?} =====");
    let (_ask, vault_fvk) = single_key_vault();
    let fx = fixture(pool, vault_fvk.clone());
    let pczt = payout::io_finalize(fx.built.pczt.clone());
    // What a signer receives: the serialized PCZT, re-parsed.
    let pczt = pczt::Pczt::parse(&pczt.serialize().unwrap()).unwrap();
    let sighash = payout::shielded_sighash(&pczt);
    let nv = pool.bundle_version().note_version();

    inspect::with_bundle(&pczt, pool, |b| {
        checks::check_no_dummy_sk(b).unwrap();
        let mut n_dummy = 0;
        for (i, a) in b.actions().iter().enumerate() {
            // Classify WITHOUT trusting spend.value: real iff rk == ak_vault + [alpha]G.
            if checks::is_vault_spend(a, &vault_fvk) {
                checks::check_real_spend(a, &vault_fvk, &fx.head).unwrap();
                checks::check_cv_net(a, fx.head.value().inner()).unwrap();
                // The dummy rule must reject a real (non-zero) spend.
                let e = checks::check_dummy_zero_value(a, nv).unwrap_err();
                println!("action {i}: real vault spend; dummy rule on it -> Err({e})");
            } else {
                n_dummy += 1;
                checks::check_dummy_zero_value(a, nv).expect("dummy is zero-valued");
                checks::check_dummy_sig(a, &sighash).expect("dummy sig verifies under rk");
                // The PCZT also carries the dummy's own fvk, so orchard's own verifiers work:
                a.spend().verify_nullifier(None).expect("dummy nullifier consistent with its fvk");
                a.spend().verify_rk(None).expect("dummy rk = ak_dummy + alpha");
                a.verify_cv_net().expect("orchard verify_cv_net");
                println!(
                    "action {i}: dummy spend; cv_net == ValueCommit(0 - {}, rcv) OK; spend_auth_sig verifies under rk OK",
                    a.output().value().unwrap().inner()
                );
            }
        }
        assert_eq!(n_dummy, 1);
    });

    // Field presence after the Redactor drops rcv (as the pczt docs permit "for Signers who
    // don't need this information"): the zero-value check becomes impossible.
    let redacted = Redactor::new(pczt.clone())
        .redact_orchard_with(|mut r| r.redact_actions(|mut a| a.clear_rcv()))
        .redact_ironwood_with(|mut r| r.redact_actions(|mut a| a.clear_rcv()))
        .finish();
    inspect::with_bundle(&redacted, pool, |b| {
        for a in b.actions() {
            if !checks::is_vault_spend(a, &vault_fvk) {
                let e = checks::check_dummy_zero_value(a, nv).unwrap_err();
                println!("rcv redacted -> dummy check: Err({e})");
                // The signature check still works (needs only rk + sig + sighash).
                checks::check_dummy_sig(a, &sighash).unwrap();
            }
        }
    });
    // Redacting output.value or output.rseed likewise breaks it (cmx can't be recomputed).
    let redacted = Redactor::new(pczt.clone())
        .redact_orchard_with(|mut r| r.redact_actions(|mut a| a.clear_output_rseed()))
        .redact_ironwood_with(|mut r| r.redact_actions(|mut a| a.clear_output_rseed()))
        .finish();
    inspect::with_bundle(&redacted, pool, |b| {
        for a in b.actions() {
            if !checks::is_vault_spend(a, &vault_fvk) {
                println!("output.rseed redacted -> dummy check: Err({})", checks::check_dummy_zero_value(a, nv).unwrap_err());
            }
        }
    });
}

#[test]
fn dummy_zero_value_orchard() {
    run(Pool::OrchardNu62);
}

#[test]
fn dummy_zero_value_ironwood() {
    run(Pool::IronwoodNu63);
}
