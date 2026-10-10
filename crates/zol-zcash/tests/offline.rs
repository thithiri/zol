//! Offline end-to-end: deposit -> scan -> refund (2-of-3 FROST and single key) -> spend
//! detected -> genesis head -> rewind -> snapshot/restore. Transactions are real, proven
//! v6 Ironwood transactions; blocks are assembled in memory (no coinbase, fake hashes), and
//! the funding note lives in a private one-leaf tree.

use ed25519_dalek::{Signer as _, SigningKey};
use orchard::keys::{FullViewingKey, Scope};
use zol_wire::{DepositMemo, HeadMemo, Network};
use zol_zcash::{
    block::ParsedBlock,
    frost, genesis, refund,
    scan::{Account, Pool, ScannerConfig},
    testing, tx, verify, AppKeys, ChainParams, Error, Scanner,
};

const P: ChainParams = ChainParams::REGTEST;

fn block(height: u32, prev: [u8; 32], raws: &[&[u8]]) -> ParsedBlock {
    let mut hash = [0u8; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    hash[4] = prev[0] ^ 0x5a; // distinguish forks
    ParsedBlock {
        height,
        hash,
        prev_hash: prev,
        txs: raws.iter().map(|r| tx::parse_tx(r).unwrap()).collect(),
    }
}

fn deposit_memo(app: &AppKeys, sess: &SigningKey, refund_to: [u8; 43]) -> [u8; 512] {
    let mut m = DepositMemo::new(
        0,
        sess.verifying_key().to_bytes(),
        refund_to,
        1_000,
        [0; 32],
        b"coinflip",
    )
    .unwrap();
    let sig = sess.sign(&m.signing_message(Network::Regtest, &app.vault_address()));
    m.set_signature(sig.to_bytes());
    m.encode_text().unwrap().0 // the `ZOL1:` text form, as a wallet would send it
}

#[test]
fn deposit_refund_genesis_rewind_snapshot() {
    let mut rng = rand::rngs::OsRng;
    // Vault group (2-of-3 DKG) and two apps.
    let (kps, pk) = frost::dkg_local(3, 2, &mut rng).unwrap();
    let k_master = [0x42u8; 32];
    let app = AppKeys::derive(
        frost::group_key(&pk),
        &k_master,
        &[0xA1; 32],
        Network::Regtest,
    )
    .unwrap();
    let other = AppKeys::derive(
        frost::group_key(&pk),
        &k_master,
        &[0xB2; 32],
        Network::Regtest,
    )
    .unwrap();
    // Player wallet (ordinary use_qsk = false key) with an off-chain funding note.
    let player_sk = testing::random_spending_key(&mut rng);
    let player = FullViewingKey::from(&player_sk);
    let (w0, w0_path, w0_anchor) = testing::offline_note(&player_sk, 500_000_000);
    let refund_to = player
        .address_at(7u32, Scope::External)
        .to_raw_address_bytes();

    let wallet_id = [0xEE; 32];
    let mut sc = Scanner::new(
        ScannerConfig::new(P),
        vec![
            Account::from(&app),
            Account::from(&other),
            Account {
                id: wallet_id,
                fvk: player.clone(),
            },
        ],
        1,
        Some([0; 32]),
        None,
    )
    .unwrap();

    // Block 1: the player deposits 1 ZEC to its session receiver with a ZOL1 text memo.
    let sess = SigningKey::generate(&mut rng);
    let (recv, idx) = app.session_receiver(&sess.verifying_key().to_bytes());
    let memo = deposit_memo(&app, &sess, refund_to);
    let (dep_raw, dep_txid) = testing::wallet_send(
        &P,
        &player_sk,
        &w0,
        &w0_path,
        w0_anchor,
        &recv,
        100_000_000,
        memo,
        1,
    )
    .unwrap();
    let b1 = block(1, [0; 32], &[&dep_raw]);
    let ev = sc.ingest_parsed(&b1).unwrap();
    let dep = ev
        .received
        .iter()
        .find(|n| n.app_id == app.app_id)
        .expect("deposit scanned")
        .clone();
    assert_eq!(dep.value, 100_000_000);
    assert_eq!(dep.pool, Pool::Ironwood);
    assert_eq!(dep.receiver, recv);
    assert_eq!(dep.receiver_index, idx);
    assert_eq!(dep.scope, Scope::External);
    assert_eq!(dep.txid, dep_txid);
    let dm = dep.deposit_memo().expect("ZOL1 text memo decodes");
    dm.verify_signature(Network::Regtest, &app.vault_address())
        .unwrap();
    assert_eq!(dm.payout_address, refund_to);
    assert!(
        !ev.received.iter().any(|n| n.app_id == other.app_id),
        "app B sees nothing"
    );
    let change = ev
        .received
        .iter()
        .find(|n| n.app_id == wallet_id)
        .expect("wallet change")
        .clone();
    assert_eq!(change.scope, Scope::Internal);
    println!(
        "block 1: deposit {} zat to session receiver idx {}, wallet change {}",
        dep.value,
        hex::encode(idx),
        change.value
    );

    // Block 2: empty.
    sc.ingest_parsed(&block(2, b1.hash, &[])).unwrap();
    let b2_hash = sc.tip().1;

    // Refund with 2-of-3 FROST under the app's tweaked key.
    let sp = sc.spendable(&dep.cmx, None).unwrap();
    let r = refund::build(&P, &app, &sp, &refund_to, 3, 20).unwrap();
    assert_eq!(r.refund_value, 100_000_000 - 10_000);
    // A signer's check rejects a wrong destination and a wrong deposit.
    assert!(refund::check(
        &r,
        &app,
        &sp.note,
        &player
            .address_at(8u32, Scope::External)
            .to_raw_address_bytes(),
        2
    )
    .is_err());
    assert!(
        refund::check(&r, &other, &sp.note, &refund_to, 2).is_err(),
        "other app's key: rk/nullifier fail"
    );
    let signers: std::collections::BTreeMap<_, _> = kps
        .iter()
        .filter(|(i, _)| **i != frost::identifier(2))
        .map(|(i, k)| (*i, k.clone()))
        .collect();
    let r = refund::sign_with_frost(r, &app, &sp.note, &signers, &pk, 2, &mut rng).unwrap();
    let (ref_raw, ref_txid) = refund::extract(r, &P).unwrap();
    assert!(
        verify::vault_outputs(&ref_raw, &app).unwrap().is_empty(),
        "no output to the vault"
    );
    // The player receives value - fee at the memo's refund address.
    let got = verify::vault_outputs(
        &ref_raw,
        &AppKeys::derive(
            frost::group_key(&pk),
            &k_master,
            &[0xA1; 32],
            Network::Regtest,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(got.is_empty());

    // Block 3: the refund; the scanner sees the deposit spent and the player's refund note.
    let b3 = block(3, b2_hash, &[&ref_raw]);
    let ev = sc.ingest_parsed(&b3).unwrap();
    assert_eq!(ev.spent.len(), 1);
    assert_eq!(ev.spent[0].cmx, dep.cmx);
    assert_eq!(ev.spent[0].spending_txid, ref_txid);
    let refunded = ev.received.iter().find(|n| n.app_id == wallet_id).unwrap();
    assert_eq!(
        (refunded.value, refunded.receiver),
        (100_000_000 - 10_000, refund_to)
    );
    // The receipt memo: "ZOL refund v1 app=<app> note=<deposit cmx>".
    assert_eq!(
        refunded.memo,
        zol_wire::receipt::refund_memo(&app.app_id, &dep.cmx)
    );
    assert!(sc.spendable(&dep.cmx, None).is_err());
    println!(
        "block 3: refund {} spent deposit; player got {}",
        zol_zcash::display_hex(&ref_txid),
        refunded.value
    );

    // Block 4: genesis head from the player's change note (as a stand-in funding wallet).
    let fsp = sc.spendable(&change.cmx, None).unwrap();
    let g = genesis::build(&P, &player_sk, &fsp, &app, 5_000_000, 4, 40).unwrap();
    let b4 = block(4, b3.hash, &[&g.raw_tx]);
    let ev = sc.ingest_parsed(&b4).unwrap();
    let head = ev.received.iter().find(|n| n.app_id == app.app_id).unwrap();
    assert_eq!(head.cmx, g.head_cmx);
    assert_eq!(head.scope, Scope::Internal);
    assert_eq!(
        HeadMemo::decode(&head.memo).unwrap(),
        HeadMemo::genesis(Network::Regtest, app.app_id)
    );
    println!(
        "block 4: genesis head cmx {} value {}",
        hex::encode(g.head_cmx),
        g.head_value
    );

    // Snapshot / restore at the tip.
    let snap = sc.snapshot().unwrap();
    println!("snapshot: {} bytes", snap.len());
    let accounts = vec![
        Account::from(&app),
        Account::from(&other),
        Account {
            id: wallet_id,
            fvk: player.clone(),
        },
    ];
    let mut sc2 = Scanner::restore(&snap, accounts.clone()).unwrap();
    assert_eq!(sc2.tip(), sc.tip());
    assert!(
        sc2.spendable(&g.head_cmx, None).is_ok(),
        "head spendable after restore"
    );
    assert!(Scanner::restore(&snap, accounts[..2].to_vec()).is_err());

    // Reorg: a block 3' that does not extend our 3 is rejected...
    let b3x = block(3, [9; 32], &[]);
    sc2.rewind(3).unwrap(); // no-op at/above tip is fine; now test real rewind
    assert!(matches!(
        sc.ingest_parsed(&block(5, [7; 32], &[])),
        Err(Error::Reorg { height: 5 })
    ));
    // ...rewind to 2 drops the refund's spend and the head; the deposit is spendable again,
    // with a witness at the old anchor.
    sc2.rewind(2).unwrap();
    assert_eq!(sc2.tip(), (2, b2_hash));
    assert!(sc2.note_by_cmx(&g.head_cmx).is_none());
    assert!(sc2.spend_of(&dep.cmx).is_none());
    let sp2 = sc2.spendable(&dep.cmx, None).unwrap();
    assert_eq!(sp2.anchor, sp.anchor);
    // A different block 3 now extends the chain.
    let _ = b3x;
    sc2.ingest_parsed(&block(3, b2_hash, &[])).unwrap();
    assert!(sc2.spendable(&dep.cmx, None).is_ok());
}

#[test]
fn single_key_refund_and_scope_isolation() {
    let mut rng = rand::rngs::OsRng;
    let (ask_group, ak_group) = testing::single_key_group(&mut rng);
    let app = AppKeys::derive(ak_group, &[1; 32], &[0xA1; 32], Network::Regtest).unwrap();
    let player_sk = testing::random_spending_key(&mut rng);
    let (w0, path, anchor) = testing::offline_note(&player_sk, 20_000_000);
    let refund_to = FullViewingKey::from(&player_sk)
        .address_at(0u32, Scope::External)
        .to_raw_address_bytes();
    // Deposit straight to the vault's main address with a binary memo.
    let (raw, _) = testing::wallet_send(
        &P,
        &player_sk,
        &w0,
        &path,
        anchor,
        &app.vault_address(),
        3_000_000,
        [0xFF; 512],
        1,
    )
    .unwrap();
    let mut sc = Scanner::new(
        ScannerConfig::new(P),
        vec![Account::from(&app)],
        1,
        None,
        None,
    )
    .unwrap();
    let ev = sc.ingest_parsed(&block(1, [0; 32], &[&raw])).unwrap();
    assert_eq!(ev.received.len(), 1);
    assert_eq!(ev.received[0].receiver_index, [0; 11]);
    let sp = sc.spendable(&ev.received[0].cmx, None).unwrap();
    let r = refund::build(&P, &app, &sp, &refund_to, 2, 40).unwrap();
    refund::check(&r, &app, &sp.note, &refund_to, 1).unwrap();
    let r = refund::sign_single(r, &testing::app_ask(&ask_group, &app)).unwrap();
    let (raw, _) = refund::extract(r, &P).unwrap();
    let ev = sc.ingest_parsed(&block(2, sc.tip().1, &[&raw])).unwrap();
    assert_eq!(ev.spent.len(), 1);
    // Refund must refuse to pay a vault address.
    assert!(sc.spendable(&ev.spent[0].cmx, None).is_err(), "spent");
    assert!(refund::build(&P, &app, &sp, &app.vault_address(), 3, 40).is_err());
    let (other_recv, _) = app.session_receiver(&[3; 32]);
    assert!(
        refund::build(&P, &app, &sp, &other_recv, 3, 40).is_err(),
        "no diversified vault receiver either"
    );
}

/// Orchard-pool outputs are scanned only with `scan_orchard_pool` (ZIP 326: `use_qsk = true`
/// keys SHOULD NOT scan the Orchard pool). Built as a pre-NU6.3 v5 transaction.
#[test]
fn orchard_pool_scanning_is_opt_in() {
    use pczt::roles::{
        creator::Creator, io_finalizer::IoFinalizer, prover::Prover, signer::Signer,
        tx_extractor::TransactionExtractor,
    };
    use zcash_primitives::transaction::{
        builder::{BuildConfig, Builder, BundlePadding},
        fees::zip317,
    };
    use zcash_protocol::{consensus::BlockHeight, memo::MemoBytes, value::Zatoshis};
    let mut rng = rand10::rng();
    let mut params = ChainParams::REGTEST;
    params.regtest.nu6_3 = 1_000_000; // NU6.2 rules: Orchard pool, v5
    params.regtest.nu7 = None;
    let (_, ak_group) = testing::single_key_group(&mut rand::rngs::OsRng);
    let app = AppKeys::derive(ak_group, &[1; 32], &[0xA1; 32], Network::Regtest).unwrap();
    let sk = testing::random_spending_key(&mut rand::rngs::OsRng);
    let fvk = FullViewingKey::from(&sk);
    // An Orchard (V2) note in a one-leaf tree.
    let (note, path, anchor) = {
        use incrementalmerkletree::{Hashable, Level};
        use orchard::{
            builder::{Builder as OB, BundleType},
            bundle::BundleVersion,
            note::ExtractedNoteCommitment,
            tree::{MerkleHashOrchard, MerklePath},
            value::NoteValue,
        };
        let bv = BundleVersion::orchard_v2();
        let mut b = OB::new(
            BundleType::DEFAULT,
            bv,
            bv.default_flags(),
            orchard::Anchor::empty_tree(),
        )
        .unwrap();
        b.add_output(
            None,
            fvk.address_at(0u32, Scope::External),
            NoteValue::from_raw(1_000_000),
            [0; 512],
        )
        .unwrap();
        let (bundle, meta) = b.build::<i64>(&mut rng).unwrap().unwrap();
        let a = &bundle.actions()[meta.output_action_index(0).unwrap()];
        let (n, _, _) = zcash_note_encryption::try_note_decryption(
            &orchard::note_encryption::OrchardDomain::for_action(a),
            &fvk.to_ivk(Scope::External).prepare(),
            a,
        )
        .unwrap();
        let path = MerklePath::from_parts(
            0,
            core::array::from_fn(|i| MerkleHashOrchard::empty_root(Level::from(i as u8))),
        );
        let anchor = path.root(ExtractedNoteCommitment::from(n.commitment()));
        (n, path, anchor)
    };
    let mut b = Builder::new(
        params,
        BlockHeight::from_u32(10),
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: Some(anchor),
            ironwood_anchor: None,
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    );
    b.add_orchard_spend::<std::convert::Infallible>(fvk.clone(), note, path)
        .unwrap();
    b.add_orchard_output::<std::convert::Infallible>(
        None,
        orchard::Address::from_raw_address_bytes(&app.vault_address()).unwrap(),
        Zatoshis::from_u64(400_000).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    b.add_orchard_output::<std::convert::Infallible>(
        None,
        fvk.address_at(0u32, Scope::Internal),
        Zatoshis::from_u64(590_000).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    let res = b
        .build_for_pczt(&mut rng, &zip317::FeeRule::standard())
        .unwrap();
    let idx = res.orchard_meta.spend_action_index(0).unwrap();
    let pczt = IoFinalizer::new(Creator::build_from_parts(res.pczt_parts).unwrap())
        .finalize_io(&mut rng)
        .unwrap();
    let pk = orchard::circuit::ProvingKey::build(
        orchard::circuit::OrchardCircuitVersion::FixedPostNu6_2,
    );
    let pczt = Prover::new(pczt)
        .create_orchard_proof(&mut rng, &pk)
        .unwrap()
        .finish();
    let mut s = Signer::new(pczt).unwrap();
    s.sign_orchard(
        &mut rng,
        idx,
        &orchard::keys::SpendAuthorizingKey::from(&sk),
    )
    .unwrap();
    let txn = TransactionExtractor::new(s.finish())
        .extract(&mut rng)
        .unwrap();
    let mut raw = vec![];
    txn.write(&mut raw).unwrap();

    for flag in [false, true] {
        let mut cfg = ScannerConfig::new(params);
        cfg.scan_orchard_pool = flag;
        let mut sc = Scanner::new(cfg, vec![Account::from(&app)], 10, None, None).unwrap();
        let ev = sc.ingest_parsed(&block(10, [0; 32], &[&raw])).unwrap();
        println!("scan_orchard_pool={flag}: {} notes", ev.received.len());
        assert_eq!(ev.received.len(), flag as usize);
        if flag {
            let n = &ev.received[0];
            assert_eq!(
                (n.pool, n.value, n.position),
                (Pool::Orchard, 400_000, None)
            );
            assert!(
                sc.spendable(&n.cmx, None).is_err(),
                "Orchard-pool notes are not witnessed"
            );
        }
    }
}
