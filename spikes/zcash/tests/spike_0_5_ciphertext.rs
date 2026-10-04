//! Spike 0.5: ciphertext reproducibility. Re-encrypt every output from its plaintext
//! (recipient, value, rseed, rho = spend nullifier, memo) and byte-compare epk,
//! enc_ciphertext and out_ciphertext with what the PCZT carries. No proof needed.

mod common;

use common::*;
use orchard::keys::{FullViewingKey, Scope};
use pczt::roles::redactor::Redactor;
use zol_zcash_spikes::{checks, inspect, payout::{self, Output}, pool::Pool};

fn run(pool: Pool) {
    println!("\n===== spike 0.5 :: {pool:?} =====");
    let (_ask, vault_fvk) = single_key_vault();
    let throwaway = FullViewingKey::from(&random_sk());
    let pad_none_to = external_address();
    let pad_ovk_to = throwaway.address_at(0u32, Scope::External);
    let padding = vec![
        Output { to: pad_none_to, value: 0, ovk: None, memo: [0u8; 512] },
        Output { to: pad_ovk_to, value: 0, ovk: Some(throwaway.to_ovk(Scope::External)), memo: [0u8; 512] },
    ];
    let fx = fixture_with_padding(pool, vault_fvk.clone(), padding);
    let pczt = payout::io_finalize(fx.built.pczt.clone());
    let pczt = pczt::Pczt::parse(&pczt.serialize().unwrap()).unwrap();
    let nv = pool.bundle_version().note_version();
    let meta = &fx.built.meta;
    let idx_payout = meta.output_action_index(0).unwrap();
    let idx_head = meta.output_action_index(1).unwrap();
    let idx_pad_none = meta.output_action_index(2).unwrap();
    let idx_pad_ovk = meta.output_action_index(3).unwrap();

    let ovk_ext = vault_fvk.to_ovk(Scope::External);
    let ovk_int = vault_fvk.to_ovk(Scope::Internal);
    let ovk_thr = throwaway.to_ovk(Scope::External);

    inspect::with_bundle(&pczt, pool, |b| {
        println!("actions = {} (1 real spend, {} dummy spends)", b.actions().len(), b.actions().len() - 1);
        for (label, i, memo, ovk) in [
            ("payout", idx_payout, fx.payout_memo, Some(&ovk_ext)),
            ("head", idx_head, fx.head_memo, Some(&ovk_int)),
            ("padding/ovk=None", idx_pad_none, [0u8; 512], None),
            ("padding/ovk=throwaway", idx_pad_ovk, [0u8; 512], Some(&ovk_thr)),
        ] {
            let a = &b.actions()[i];
            let en = a.output().encrypted_note();
            assert!(a.output().ock().is_none(), "builder leaves pczt ock = None");
            checks::check_cmx(a, nv).unwrap();
            let (epk, enc) = checks::recompute_enc(a, nv, memo).unwrap();
            let (epk2, enc2) = checks::recompute_enc(a, nv, memo).unwrap();
            assert_eq!((epk, enc), (epk2, enc2), "deterministic");
            assert_eq!(epk, en.epk_bytes, "{label}: epk reproducible from rseed");
            assert_eq!(enc, en.enc_ciphertext, "{label}: enc_ciphertext byte-identical");
            let rec = checks::recover_with_esk(a, nv).expect("esk recovery");
            assert_eq!(rec, memo, "{label}: memo recovered with pk_d + esk(rseed)");
            let mut wrong = memo;
            wrong[511] ^= 1;
            assert_ne!(checks::recompute_enc(a, nv, wrong).unwrap().1, en.enc_ciphertext);
            let out_match: Vec<&str> = [("ovk_ext", &ovk_ext), ("ovk_int", &ovk_int), ("ovk_throwaway", &ovk_thr)]
                .iter()
                .filter(|(_, k)| checks::recompute_out(a, nv, k).unwrap() == en.out_ciphertext)
                .map(|(n, _)| *n)
                .collect();
            println!(
                "{label:22} action {i}: epk OK, enc_ciphertext(580B) OK, memo-by-esk OK; out_ciphertext reproduced by {:?}",
                out_match
            );
            match ovk {
                Some(k) => {
                    assert_eq!(checks::recompute_out(a, nv, k).unwrap(), en.out_ciphertext, "{label}: out_ciphertext");
                    assert_eq!(out_match.len(), 1);
                    let _ock = checks::derive_ock(a, k);
                }
                None => assert!(out_match.is_empty(), "ovk=None -> random out_ciphertext"),
            }
        }
    });

    // The pczt crate itself relies on reproducibility: the v2 encoding may replace
    // enc_ciphertext by the memo plaintext and recompute it when parsing.
    let compact = Redactor::new(pczt.clone())
        .redact_orchard_with(|mut r| r.compact_resolvable_fields())
        .redact_ironwood_with(|mut r| r.compact_resolvable_fields())
        .finish();
    let bytes = compact.clone().serialize().unwrap();
    let n_memo = match pool {
        Pool::OrchardNu62 => compact.orchard(),
        Pool::IronwoodNu63 => compact.ironwood(),
    }
    .actions()
    .iter()
    .filter(|a| matches!(a.output().enc_ciphertext(), pczt::orchard::EncCiphertext::MemoPlaintext(_)))
    .count();
    let mut resolved = pczt::Pczt::parse(&bytes).unwrap();
    resolved.resolve_fields().unwrap();
    let orig = inspect::with_bundle(&pczt, pool, |b| b.actions().iter().map(|a| a.output().encrypted_note().enc_ciphertext).collect::<Vec<_>>());
    let res = inspect::with_bundle(&resolved, pool, |b| b.actions().iter().map(|a| a.output().encrypted_note().enc_ciphertext).collect::<Vec<_>>());
    assert_eq!(orig, res);
    println!(
        "pczt compact encoding: {} of {} outputs carried as memo plaintext ({} bytes), re-encrypted on parse byte-identically",
        n_memo,
        orig.len(),
        bytes.len()
    );
}

#[test]
fn ciphertext_orchard() {
    run(Pool::OrchardNu62);
}

#[test]
fn ciphertext_ironwood() {
    run(Pool::IronwoodNu63);
}
