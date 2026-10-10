//! Offline head transitions: honest payout and consolidation through the head chain, and a
//! malicious coordinator's proposals that every operator's verifier must reject by name.

use std::collections::BTreeMap;

use ff::PrimeField;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use pczt::Pczt;
use zol_types::api::{PayoutLine, PayoutPlan, PayoutRecord};
use zol_wire::{HeadMemo, Network, TopUpMemo, TopUpPurpose};
use zol_zcash::{
    block::ParsedBlock,
    frost::{self, KeyPackages, PublicKeyPackage},
    head,
    payout::{self, Tamper},
    scan::{Account, ScannerConfig},
    testing, tx, AppKeys, ChainParams, Scanner,
};

const P: ChainParams = ChainParams::REGTEST;

fn block(height: u32, prev: [u8; 32], raws: &[&[u8]]) -> ParsedBlock {
    let mut hash = [0u8; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    hash[4] = 0xAB;
    ParsedBlock {
        height,
        hash,
        prev_hash: prev,
        txs: raws.iter().map(|r| tx::parse_tx(r).unwrap()).collect(),
    }
}

struct World {
    sc: Scanner,
    a: AppKeys,
    b: AppKeys,
    w_sk: SpendingKey,
    w_fvk: FullViewingKey,
    kps: KeyPackages,
    pk: PublicKeyPackage,
    genesis_a: [u8; 32],
    genesis_b: [u8; 32],
    d1: [u8; 32],
    d2: [u8; 32],
    topup: [u8; 32],
    p1: [u8; 43],
}

const WALLET: [u8; 32] = [0xEE; 32];

impl World {
    /// Ingests a block with `raws` on top of the tip.
    fn mine(&mut self, raws: &[&[u8]]) {
        let (h, hash) = self.sc.tip();
        self.sc.ingest_parsed(&block(h + 1, hash, raws)).unwrap();
    }
    /// The wallet's largest unspent note.
    fn wallet_note(&self) -> zol_zcash::scan::Spendable {
        let n = self
            .sc
            .unspent(&WALLET)
            .max_by_key(|n| n.value)
            .unwrap()
            .cmx;
        self.sc.spendable(&n, None).unwrap()
    }
    fn send(&mut self, to: [u8; 43], value: u64, memo: [u8; 512]) -> [u8; 32] {
        let sp = self.wallet_note();
        let (raw, _) = testing::wallet_send(
            &P,
            &self.w_sk,
            &sp.note,
            &sp.path,
            sp.anchor,
            &to,
            value,
            memo,
            self.sc.next_height(),
        )
        .unwrap();
        self.mine(&[&raw]);
        let txid = *tx::parse_tx(&raw).unwrap().txid().as_ref();
        self.sc
            .notes_in_tx(&txid)
            .iter()
            .find(|n| n.receiver == to)
            .unwrap()
            .cmx
    }
    fn plan(
        &self,
        app: &AppKeys,
        genesis: [u8; 32],
        seq: u64,
        credited: Vec<[u8; 32]>,
        amount: u64,
    ) -> PayoutPlan {
        PayoutPlan {
            app: app.app_id,
            genesis_head: genesis,
            records: vec![PayoutRecord {
                sequence: seq,
                payouts: vec![PayoutLine {
                    commitment: [seq as u8 + 1; 32],
                    amount,
                    to: self.p1.to_vec(),
                }],
            }],
            credited,
            fee_pool_accrued: 100_000,
            claims_after: 0,
            action_target: 0,
            max_inputs: 4,
            full_depth: 1,
            head_depth: 1,
            expiry_delta: 40,
        }
    }
    /// Signs every vault spend with 2-of-3 FROST and extracts.
    fn sign(&self, keys: &AppKeys, pczt: Pczt) -> Vec<u8> {
        let mut akps = BTreeMap::new();
        let mut apk = None;
        for (i, kp) in &self.kps {
            let (a, p) = frost::app_packages(keys, kp, &self.pk).unwrap();
            akps.insert(*i, a);
            apk = Some(p);
        }
        let apk = apk.unwrap();
        let sighash = tx::sighash(&pczt).unwrap();
        let mut pczt = pczt;
        for (idx, alpha) in payout::vault_spends(keys, &pczt).unwrap() {
            let sig = frost::sign_locally(
                &akps,
                &apk,
                &[frost::identifier(1), frost::identifier(3)],
                &alpha,
                &sighash,
                &mut rand::rngs::OsRng,
            )
            .unwrap();
            pczt = tx::apply_signature(pczt, idx as usize, sig).unwrap();
        }
        tx::extract(pczt, &P).unwrap().0
    }
}

fn world() -> World {
    let mut rng = rand::rngs::OsRng;
    let (kps, pk) = frost::dkg_local(3, 2, &mut rng).unwrap();
    let a = AppKeys::derive(
        frost::group_key(&pk),
        &[7; 32],
        &[0xA1; 32],
        Network::Regtest,
    )
    .unwrap();
    let b = AppKeys::derive(
        frost::group_key(&pk),
        &[7; 32],
        &[0xB2; 32],
        Network::Regtest,
    )
    .unwrap();
    let w_sk = testing::random_spending_key(&mut rng);
    let w_fvk = FullViewingKey::from(&w_sk);
    let (n0, path, anchor) = testing::offline_note(&w_sk, 1_000_000_000);
    let mut sc = Scanner::new(
        ScannerConfig::new(P),
        vec![
            Account::from(&a),
            Account::from(&b),
            Account {
                id: WALLET,
                fvk: w_fvk.clone(),
            },
        ],
        1,
        Some([0; 32]),
        None,
    )
    .unwrap();
    // Block 1: app A genesis (50 ZEC float... 0.5 ZEC) from the offline funding note.
    let (raw, _) = testing::wallet_send(
        &P,
        &w_sk,
        &n0,
        &path,
        anchor,
        &a.vault_internal_address(),
        50_000_000,
        HeadMemo::genesis(Network::Regtest, a.app_id).encode(),
        1,
    )
    .unwrap();
    sc.ingest_parsed(&block(1, [0; 32], &[&raw])).unwrap();
    let p1 = FullViewingKey::from(&testing::random_spending_key(&mut rng))
        .address_at(0u32, Scope::External)
        .to_raw_address_bytes();
    let mut w = World {
        sc,
        a,
        b,
        w_sk,
        w_fvk,
        kps,
        pk,
        genesis_a: [0; 32],
        genesis_b: [0; 32],
        d1: [0; 32],
        d2: [0; 32],
        topup: [0; 32],
        p1,
    };
    w.genesis_a = w.sc.unspent(&w.a.app_id).next().unwrap().cmx;
    let (bi, bm) = (
        w.b.vault_internal_address(),
        HeadMemo::genesis(Network::Regtest, w.b.app_id).encode(),
    );
    w.genesis_b = w.send(bi, 50_000_000, bm);
    let (r1, _) = w.a.session_receiver(&[1; 32]);
    w.d1 = w.send(r1, 30_000_000, [0xFF; 512]);
    let (r2, _) = w.a.session_receiver(&[2; 32]);
    w.d2 = w.send(r2, 20_000_000, [0xFF; 512]);
    let va = w.a.vault_address();
    w.topup = w.send(
        va,
        1_000_000,
        TopUpMemo {
            network: Network::Regtest,
            app: w.a.app_id,
            purpose: TopUpPurpose::FeePool,
        }
        .encode(),
    );
    w
}

fn names(v: &zol_types::api::VerifyResponse) -> String {
    v.failures.join(",")
}

#[test]
fn honest_payout_then_consolidation_advance_the_head() {
    let mut w = world();
    let walk = head::walk(&w.sc, &w.a, &w.genesis_a);
    assert!(walk.fault.is_none() && walk.links.len() == 1);

    // Payout of record 0 (25M to p1), credited D1.
    let plan = w.plan(&w.a, w.genesis_a, 0, vec![w.d1], 60_000_000);
    let (prop, pczt) = payout::propose(&P, &w.a, &w.sc, &plan, &Tamper::default()).unwrap();
    let v = payout::verify(&w.a, &w.sc, &plan, &prop.pczt, None);
    assert!(v.ok, "honest proposal: {}", names(&v));
    assert_eq!(v.sighash, prop.sighash);
    println!(
        "payout: inputs {} fee {} new head {}",
        prop.inputs.len(),
        prop.fee,
        prop.new_head_value
    );
    // A second, different proposal for the same record spends the same head nullifier.
    let (prop2, pczt2) = payout::propose(&P, &w.a, &w.sc, &plan, &Tamper::default()).unwrap();
    assert!(payout::verify(&w.a, &w.sc, &plan, &prop2.pczt, None).ok);
    assert_ne!(prop.sighash, prop2.sighash);
    let head_nf = w.sc.note_by_cmx(&w.genesis_a).unwrap().nullifier;
    let nfs = |p: &Pczt| {
        tx::with_ironwood(p, |b| {
            b.actions()
                .iter()
                .map(|a| a.spend().nullifier().to_bytes())
                .collect::<Vec<_>>()
        })
        .unwrap()
    };
    assert!(
        nfs(&pczt).contains(&head_nf) && nfs(&pczt2).contains(&head_nf),
        "both spend the head: only one can confirm"
    );
    // While a different transition is pending, the verifier refuses.
    assert!(names(&payout::verify(
        &w.a,
        &w.sc,
        &plan,
        &prop2.pczt,
        Some(prop.sighash)
    ))
    .contains("head_pending"));

    let raw = w.sign(&w.a, pczt);
    w.mine(&[&raw]);
    let walk = head::walk(&w.sc, &w.a, &w.genesis_a);
    assert!(walk.fault.is_none(), "{:?}", walk.fault);
    assert_eq!(walk.links.len(), 2);
    let h1 = walk.links[1].memo;
    assert_eq!(
        (h1.position, h1.paid_count, h1.network_fees_spent),
        (1, 1, prop.fee)
    );
    assert_eq!(walk.links[1].note.value, prop.new_head_value);
    // Paying record 0 again is refused: the next record is 1.
    let again = payout::verify(&w.a, &w.sc, &plan, &prop2.pczt, None);
    assert!(
        names(&again).contains("records_not_next"),
        "{}",
        names(&again)
    );

    // Consolidation: no records; sweeps D2 (credited) and the fee top-up.
    let mut cons = w.plan(&w.a, w.genesis_a, 1, vec![w.d2], 1);
    cons.records.clear();
    cons.action_target = 4;
    let (cp, cpczt) = payout::propose(&P, &w.a, &w.sc, &cons, &Tamper::default()).unwrap();
    let v = payout::verify(&w.a, &w.sc, &cons, &cp.pczt, None);
    assert!(v.ok, "consolidation: {}", names(&v));
    assert!(cp.inputs.contains(&w.d2) && cp.inputs.contains(&w.topup));
    let raw = w.sign(&w.a, cpczt);
    w.mine(&[&raw]);
    let walk = head::walk(&w.sc, &w.a, &w.genesis_a);
    assert!(walk.fault.is_none(), "{:?}", walk.fault);
    let h2 = walk.links[2].memo;
    assert_eq!(
        (h2.position, h2.paid_count, h2.fee_topups_swept),
        (2, 1, 1_000_000)
    );
    assert_eq!(h2.network_fees_spent, prop.fee + cp.fee);
    assert_eq!(cp.fee, 4 * 5_000, "padded to 4 actions");
    println!(
        "consolidation: head {} -> {}, swept top-up 1,000,000, fee {}",
        walk.links[1].note.value, walk.links[2].note.value, cp.fee
    );
}

#[test]
fn malicious_coordinator_is_rejected_by_name() {
    let w = world();
    let plan = w.plan(&w.a, w.genesis_a, 0, vec![w.d1], 60_000_000);
    let check = |label: &str, t: Tamper, expect: &str| {
        let (prop, _) = payout::propose(&P, &w.a, &w.sc, &plan, &t).unwrap();
        let v = payout::verify(&w.a, &w.sc, &plan, &prop.pczt, None);
        println!("{label:28} -> {}", names(&v));
        assert!(
            !v.ok && v.failures.iter().any(|f| f == expect),
            "{label}: expected {expect}, got {}",
            names(&v)
        );
    };
    let other = FullViewingKey::from(&testing::random_spending_key(&mut rand::rngs::OsRng))
        .address_at(0u32, Scope::External)
        .to_raw_address_bytes();
    check(
        "extra payout",
        Tamper {
            extra_payout: Some((other, 1_000_000)),
            ..Default::default()
        },
        "unexpected_value_output",
    );
    check(
        "missing payout",
        Tamper {
            drop_first_payout: true,
            ..Default::default()
        },
        "payout_missing",
    );
    check(
        "wrong amount",
        Tamper {
            first_amount_delta: -1_000,
            ..Default::default()
        },
        "payout_missing",
    );
    check(
        "wrong receiver",
        Tamper {
            first_receiver: Some(other),
            ..Default::default()
        },
        "unexpected_value_output",
    );
    let d2 = w.sc.spendable(&w.d2, None).unwrap();
    check(
        "non-credited note",
        Tamper {
            extra_inputs: vec![(w.a.fvk().clone(), d2)],
            ..Default::default()
        },
        "spend_not_allowed",
    );
    check(
        "two vault outputs",
        Tamper {
            extra_vault_output: true,
            ..Default::default()
        },
        "vault_outputs_count",
    );
    let wn = w.wallet_note();
    check(
        "non-zero dummy",
        Tamper {
            extra_inputs: vec![(w.w_fvk.clone(), wn)],
            ..Default::default()
        },
        "dummy_nonzero",
    );
    // 1 spend (the head covers 25M) and 2 outputs: the builder adds one dummy spend.
    let small = w.plan(&w.a, w.genesis_a, 0, vec![w.d1], 25_000_000);
    let (p_small, _) = payout::propose(
        &P,
        &w.a,
        &w.sc,
        &small,
        &Tamper {
            skip_io_finalize: true,
            ..Default::default()
        },
    )
    .unwrap();
    let v = payout::verify(&w.a, &w.sc, &small, &p_small.pczt, None);
    println!("{:28} -> {}", "leftover dummy key", names(&v));
    assert!(v.failures.iter().any(|f| f == "dummy_key_present"));
    let head = head::walk(&w.sc, &w.a, &w.genesis_a).links[0].memo;
    let bad_memo = head
        .advance(&zol_wire::Transition {
            records_paid: 2,
            records_digest: [9; 32],
            network_fee: 10_000,
            fee_topups_swept: 0,
        })
        .unwrap();
    check(
        "wrong memo advance",
        Tamper {
            memo_override: Some(bad_memo),
            ..Default::default()
        },
        "head_memo_advance",
    );
    // Built for the pre-NU7 branch (NU6.3) while the chain is on NU7: never mineable.
    let pre_nu7 = ChainParams {
        regtest: zol_zcash::params::RegtestHeights::PRE_NU7,
        ..P
    };
    let (p_stale, _) = payout::propose(&pre_nu7, &w.a, &w.sc, &plan, &Tamper::default()).unwrap();
    let v = payout::verify(&w.a, &w.sc, &plan, &p_stale.pczt, None);
    println!("{:28} -> {}", "pre-NU7 branch", names(&v));
    assert!(v.failures.iter().any(|f| f == "branch"));

    // Same honest proposal, judged against an operator's own stricter plan.
    let (prop, pczt) = payout::propose(&P, &w.a, &w.sc, &plan, &Tamper::default()).unwrap();
    let own = |f: &dyn Fn(&mut PayoutPlan), expect: &str| {
        let mut p = plan.clone();
        f(&mut p);
        let v = payout::verify(&w.a, &w.sc, &p, &prop.pczt, None);
        println!("own plan: {expect:22} -> {}", names(&v));
        assert!(
            v.failures.iter().any(|x| x == expect),
            "{expect}: {}",
            names(&v)
        );
    };
    own(&|p| p.fee_pool_accrued = 5_000, "fee_pool");
    own(&|p| p.claims_after = 1_000_000_000, "reserve");
    own(&|p| p.records[0].sequence = 1, "records_not_next");
    own(&|p| p.credited.clear(), "spend_not_allowed");
    own(&|p| p.records[0].payouts[0].amount += 1, "payout_missing");

    // Tampered ciphertext (one byte of the payout's enc_ciphertext).
    let enc = tx::with_ironwood(&pczt, |b| {
        b.actions()
            .iter()
            .find(|a| a.output().recipient().unwrap().to_raw_address_bytes() == w.p1)
            .unwrap()
            .output()
            .encrypted_note()
            .enc_ciphertext
    })
    .unwrap();
    let mut bytes = prop.pczt.clone();
    let at = bytes.windows(580).position(|x| x == enc).unwrap();
    bytes[at + 300] ^= 1;
    let v = payout::verify(&w.a, &w.sc, &plan, &bytes, None);
    println!("tampered ciphertext           -> {}", names(&v));
    assert!(v.failures.iter().any(|f| f == "enc_ciphertext"));

    // Redacted rseed.
    let red = pczt::roles::redactor::Redactor::new(pczt.clone())
        .redact_ironwood_with(|mut r| r.redact_action(0, |mut a| a.clear_output_rseed()))
        .finish();
    let v = payout::verify(&w.a, &w.sc, &plan, &red.serialize().unwrap(), None);
    println!("redacted rseed                -> {}", names(&v));
    assert!(v.failures.iter().any(|f| f == "redacted_field"));

    // Cross-app (spike 0.8): app B's head transition relabelled as app A's with a shifted alpha.
    let bplan = w.plan(&w.b, w.genesis_b, 0, vec![], 25_000_000);
    let (bprop, bpczt) = payout::propose(&P, &w.b, &w.sc, &bplan, &Tamper::default()).unwrap();
    let (idx, alpha_b) = payout::vault_spends(&w.b, &bpczt).unwrap()[0];
    let ab = pasta_curves::pallas::Scalar::from_repr(alpha_b).unwrap();
    let shifted = ab + w.b.tweak_scalar() - w.a.tweak_scalar();
    let mut bytes = bprop.pczt.clone();
    let at = bytes.windows(32).position(|x| x == alpha_b).unwrap();
    bytes[at..at + 32].copy_from_slice(&shifted.to_repr());
    let relabeled = Pczt::parse(&bytes).unwrap();
    assert_eq!(
        payout::vault_spends(&w.a, &relabeled).unwrap()[0].0,
        idx,
        "rk test alone says: app A spend"
    );
    let v = payout::verify(&w.a, &w.sc, &plan, &bytes, None);
    println!("app B head, shifted alpha     -> {}", names(&v));
    assert!(v.failures.iter().any(|f| f == "real_spend_unknown_note"));
    assert!(v.failures.iter().any(|f| f == "head_not_spent"));
}
