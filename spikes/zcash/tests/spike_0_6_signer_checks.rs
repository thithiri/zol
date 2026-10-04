//! Spike 0.6: what the pczt Signer role checks by itself (probes), and a demonstration
//! that a ciphertext swap passes every pczt role and consensus verification while
//! burning the payout; the gateway's recompute check catches it.

mod common;

use common::*;
use orchard::keys::Scope;
use pczt::roles::{redactor::Redactor, signer::Signer};
use zol_zcash_spikes::{checks, inspect, payout, pool::Pool, verify};

/// pczt's Signer does not reject a PCZT that still carries dummy_sk (the "Signers MUST
/// reject" rule in the field docs is not enforced by Signer::new / sign_*).
#[test]
fn signer_accepts_pczt_with_dummy_sk() {
    let pool = Pool::IronwoodNu63;
    let (ask, vault_fvk) = single_key_vault();
    let fx = fixture(pool, vault_fvk);
    let pczt = fx.built.pczt.clone(); // Creator stage: dummy_sk present
    let has_dummy_sk = inspect::with_bundle(&pczt, pool, |b| checks::check_no_dummy_sk(b).is_err());
    assert!(has_dummy_sk);
    let mut s = Signer::new(pczt).expect("Signer::new accepts a PCZT with dummy_sk");
    s.sign_ironwood(fx.built.meta.spend_action_index(0).unwrap(), &ask)
        .expect("and signs the real spend");
    let signed = s.finish();
    let still = inspect::with_bundle(&signed, pool, |b| checks::check_no_dummy_sk(b).is_err());
    println!("Signer::new + sign with dummy_sk present: Ok; dummy_sk still present after signing: {still}");
}

#[test]
fn signer_probes() {
    let pool = Pool::OrchardNu62;
    let (ask, vault_fvk) = single_key_vault();
    let fx = fixture(pool, vault_fvk);
    let pczt = payout::io_finalize(fx.built.pczt.clone());
    let real = fx.built.meta.spend_action_index(0).unwrap();
    let dummy = 1 - real;

    // Wrong key -> rejected (rk check: ask.randomize(alpha) must give rk).
    let (other_ask, _) = single_key_vault();
    let mut s = Signer::new(pczt.clone()).unwrap();
    println!("sign with wrong ask: {:?}", s.sign_orchard(real, &other_ask).map(|_| ()));
    // Signing the dummy action with the vault key -> rejected.
    println!("sign dummy action with vault ask: {:?}", s.sign_orchard(dummy, &ask).map(|_| ()));
    // External garbage signature -> rejected (verified under rk before storing).
    let r = payout::apply_signature(pczt.clone(), pool, real, [7u8; 64]);
    println!("apply garbage external signature: {:?}", r.as_ref().map(|_| ()));
    assert!(r.is_err());

    // rcv redacted: the Signer still signs (it never checks cv_net).
    let redacted = Redactor::new(pczt.clone())
        .redact_orchard_with(|mut r| r.redact_actions(|mut a| a.clear_rcv()))
        .finish();
    let mut s = Signer::new(redacted).unwrap();
    println!("sign with every rcv redacted: {:?}", s.sign_orchard(real, &ask).map(|_| ()));

    // Note fields of the spent note redacted: verify_nullifier is skipped, Signer signs.
    let redacted = Redactor::new(pczt.clone())
        .redact_orchard_with(|mut r| r.redact_actions(|mut a| { a.clear_spend_value(); a.clear_spend_rseed(); }))
        .finish();
    let mut s = Signer::new(redacted).unwrap();
    println!("sign with spend.value + spend.rseed redacted (nullifier unverifiable): {:?}", s.sign_orchard(real, &ask).map(|_| ()));
}

fn find(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    hay.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).map(|(i, _)| i).collect()
}

/// A coordinator swaps one byte of the payout's enc_ciphertext. Every pczt role accepts
/// it, the extracted transaction is consensus-valid (proof, spend-auth and binding
/// signatures all verify), but the recipient cannot decrypt: the payout is burned.
#[test]
fn tampered_ciphertext_burns_payout_unless_gateway_recomputes() {
    for pool in Pool::ALL {
        println!("\n--- {pool:?}");
        let (ask, vault_fvk) = single_key_vault();
        let fx = fixture(pool, vault_fvk.clone());
        let real = fx.built.meta.spend_action_index(0).unwrap();
        let pay = fx.built.meta.output_action_index(0).unwrap();
        let nv = pool.bundle_version().note_version();
        // (1) Tampering AFTER IoFinalizer (e.g. by a relay) is caught by the pczt itself:
        // IoFinalizer's dummy-spend signatures commit to the original effects, so
        // TxExtractor fails with SighashMismatch.
        {
            let p = payout::prove(payout::io_finalize(fx.built.pczt.clone()), pool);
            let enc = inspect::with_bundle(&p, pool, |b| b.actions()[pay].output().encrypted_note().enc_ciphertext);
            let mut bytes = p.serialize().unwrap();
            let at = find(&bytes, &enc);
            assert_eq!(at.len(), 1);
            bytes[at[0] + 300] ^= 0x01;
            let t = pczt::Pczt::parse(&bytes).unwrap();
            let r = payout::extract(payout::sign_single(t, pool, real, &ask), pool);
            println!("tamper after IoFinalizer -> TxExtractor: {:?}", r.as_ref().map(|_| ()));
            assert!(r.is_err());
        }

        // (2) A malicious COORDINATOR tampers before IoFinalizer (it runs IoFinalizer itself).
        let enc = inspect::with_bundle(&fx.built.pczt, pool, |b| b.actions()[pay].output().encrypted_note().enc_ciphertext);
        let mut bytes = fx.built.pczt.clone().serialize().unwrap();
        let at = find(&bytes, &enc);
        assert_eq!(at.len(), 1, "enc_ciphertext appears once in the encoding");
        bytes[at[0] + 300] ^= 0x01; // inside the memo part of the ciphertext
        let tampered = pczt::Pczt::parse(&bytes).expect("tampered PCZT parses");
        let tampered = payout::prove(payout::io_finalize(tampered), pool);

        // Gateway recompute check catches it.
        inspect::with_bundle(&tampered, pool, |b| {
            let a = &b.actions()[pay];
            checks::check_cmx(a, nv).expect("cmx still fine (ciphertext is not committed)");
            let (_, re) = checks::recompute_enc(a, nv, fx.payout_memo).unwrap();
            assert_ne!(re, a.output().encrypted_note().enc_ciphertext);
            assert!(checks::recover_with_esk(a, nv).is_none());
            println!("gateway: recomputed enc_ciphertext != proposal; esk-decryption fails -> REJECT");
        });

        // ...but pczt roles and consensus verification accept it.
        let signed = payout::finalize_spends(payout::sign_single(tampered, pool, real, &ask));
        let tx = payout::extract(signed, pool).expect("TxExtractor accepts (it verified proof + sigs)");
        let raw = verify::raw(&tx);
        let rep = verify::verify_raw(&raw, pool).expect("consensus-level verification passes");
        println!("pczt Signer signed it; TxExtractor + independent verify OK (txid {})", rep.txid);
        let bundle = match pool {
            Pool::OrchardNu62 => tx.orchard_bundle(),
            Pool::IronwoodNu63 => tx.ironwood_bundle(),
        }
        .unwrap();
        let got = bundle.decrypt_outputs_with_keys(&[fx.payout_fvk.to_ivk(Scope::External)]);
        assert!(got.is_empty());
        println!("recipient trial-decryption of the mined payout: none -> 600000 zat burned");
    }
}
