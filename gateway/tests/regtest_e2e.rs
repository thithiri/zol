//! Phase 3 end to end, across processes (ignored by default):
//!
//! Zakura regtest  ←→  3 × zol-zcashd (one per operator, own scan)  ←→  gateway
//! cluster (3 nodes, 2-of-3 FROST attestor)  ←→  ledger program on LiteSVM.
//!
//! Needs the regtest pair (`infra/zakura/scripts/up.sh pair`; its miner address is
//! the "zol-dev" dev wallet the daemons fund from) and a built
//! `zol-zcashd` (`cargo build --release --manifest-path crates/zol-zcash/Cargo.toml --bin zol-zcashd`):
//!
//!     cargo test -p zol-gateway --test regtest_e2e -- --ignored --nocapture --test-threads=1
//!
//! Env: ZOL_NODE_URL (default http://127.0.0.1:18232/), ZOL_DEV_SEED (default zol-dev),
//! ZOL_ZCASHD_BIN.

mod common;


use anchor_lang::prelude::Pubkey;
use common::*;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use zol_gateway::attestor::dkg;
use zol_gateway::chain::ChainEvent;
use zol_gateway::ledger::Ledger;
use zol_gateway::node::{AppRuntime, Node};
use zol_gateway::service::{Cluster, Operator};
use zol_gateway::store::Store;
use zol_gateway::zcashd::OrchardReceivers;
use zol_ledger::state::{AppState, DepositState};
use zol_tests::*;
use zol_wire::Network;

use common::regtest::*;

struct Setup {
    _procs: (Proc, Proc, Proc),
    world: World,
    cluster: Cluster,
    vault_addr: [u8; 43],
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn sync(c: &Cluster) {
    wait_synced(&c.operators.iter().map(|o| &o.zcashd).collect::<Vec<_>>());
}

/// Three single-share dev daemons, a genesis head, the ledger with the app
/// activated after the guardians' genesis check, and the gateway cluster.
fn setup() -> Setup {
    let start_height = node_height();
    let (p0, z0) = spawn_zcashd(0, free_port(), start_height);
    let (p1, z1) = spawn_zcashd(1, free_port(), start_height);
    let (p2, z2) = spawn_zcashd(2, free_port(), start_height);
    let _procs = (p0, p1, p2);

    // Same dev seed: same vault on every operator.
    let vault = z0.vault(&app_id()).unwrap();
    assert_eq!(z1.vault(&app_id()).unwrap(), vault);
    let vault_addr: [u8; 43] = vault.address.as_slice().try_into().unwrap();

    // Genesis head, then the ledger activates the app with it.
    let g = z0.dev_genesis(&app_id(), 30_000_000).unwrap();
    mine(1);
    let mut env = Env::bare();
    env.initialize_network(Network::Regtest as u8);
    ok!(env.register_test_app());
    ok!(env.activate_with(&TEST_APP, vault_addr, g.head_cmx));
    ok!(env.set_cap(&TEST_APP, u64::MAX / 4));

    let (keys, pubkeys) = dkg(3, 2, &mut OsRng).unwrap();
    let mut world = World::with_env(env);
    // Replace the scripted-chain nodes with zcashd-backed operators on regtest.
    let seeds: Vec<[u8; 32]> = world.ledger.env.guardians.iter().map(|k| *k.secret_bytes()).collect();
    world.ledger.group_key = zol_gateway::attestor::group_key(&pubkeys);
    ok!(world.ledger.env.set_config(zol_ledger::SetConfigArgs {
        new_admin: None,
        attestor: Some(Pubkey::new_from_array(world.ledger.group_key)),
        params: None,
        paused: None,
    }));
    let mut cfg = gw_config();
    cfg.network = Network::Regtest as u8;
    cfg.early_credit_cap = 10_000_000;
    let clients = [z0, z1, z2];
    let operators: Vec<Operator> = keys
        .into_iter()
        .zip(clients)
        .enumerate()
        .map(|(i, (k, z))| {
            let mut apps = std::collections::BTreeMap::new();
            let app = app_id();
            apps.insert(
                app,
                AppRuntime {
                    program: TEST_APP,
                    vault_address: vault_addr,
                    secrets: zol_gateway::config::AppSecrets::derive(&MASTER, Network::Regtest, &app),
                    session_index: Box::new(move |s: &[u8; 32]| zol_types::session_receiver_index(&app, s)),
                },
            );
            let node = Node {
                heads: Default::default(),
                name: format!("op{i}"),
                store: Store::in_memory().unwrap(),
                cfg: cfg.clone(),
                attestor: zol_gateway::attestor::Attestor::new(k, pubkeys.clone(), Network::Regtest),
                guardian_seed: seeds[i],
                apps,
                addresses: Box::new(OrchardReceivers),
            };
            Operator::new(node, z).unwrap()
        })
        .collect();
    let mut cluster = Cluster { operators, pubkeys, threshold: 2, coordinator: 0, session: 0, attempts: Default::default() };

    // During the activation delay guardians check the genesis head: shallow first.
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert!(r.guard.iter().any(|(_, a)| format!("{a:?}").contains("Shallow")), "{:?}", r.guard);
    assert_eq!(world.ledger.env.app_state().state, AppState::Pending, "a shallow genesis head cancels activation");
    // Re-activate once the head is at full depth.
    mine(100);
    ok!(world.ledger.env.activate_with(&TEST_APP, vault_addr, g.head_cmx));
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert!(r.guard.is_empty(), "{:?}", r.guard);
    world.ledger.env.warp(params().activation_delay_secs);

    Setup { _procs, world, cluster, vault_addr }
}

#[test]
#[ignore]
fn phase3_end_to_end_on_regtest() {
    let Setup { _procs, mut world, mut cluster, vault_addr } = setup();
    // A player deposits through an ordinary wallet send to their session receiver,
    // with the ZOL1 text memo a payment link carries. Another deposit's memo is expired.
    let h = node_height();
    let alice = SigningKey::from_bytes(&[0xA1; 32]);
    let bob = SigningKey::from_bytes(&[0xB0; 32]);
    let refund_to = external_receiver();
    let ma = session_memo(&alice, &vault_addr, h + 1_000, refund_to);
    let mb = session_memo(&bob, &vault_addr, h + 3, refund_to);
    let ra = cluster.operators[0].zcashd.session_receiver(&app_id(), &ma.session_key).unwrap();
    assert_eq!(ra.index, zol_types::session_receiver_index(&app_id(), &ma.session_key), "index formula matches zcashd");
    let rb = cluster.operators[0].zcashd.session_receiver(&app_id(), &mb.session_key).unwrap();
    cluster.operators[0].zcashd.dev_send(&ra.receiver, 1_000_000, &ma.encode_text().unwrap().0).unwrap();
    cluster.operators[0].zcashd.dev_send(&rb.receiver, 1_000_000, &mb.encode()).unwrap();
    mine(10);
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    println!("round: credited={} voided={} failed={:?}", r.round.credited.len(), r.round.voided.len(), r.round.failed);
    assert_eq!((r.round.credited.len(), r.round.voided.len()), (1, 1), "{:?}", r.round);
    let bal = world.ledger.env.balance(&Pubkey::new_from_array(ma.session_key));
    assert_eq!(bal.amount, 1_000_000 - 1_000_000 * 30 / 10_000);
    let voided = r.round.voided[0];
    assert_eq!(world.ledger.deposit(&voided).unwrap().unwrap().state, DepositState::Voided);
    world.ledger.env.check_invariants();

    // In the same tick the void triggered a refund, signed 2-of-3 by single-share
    // operators each verifying it against its own view; it is pending, so the
    // next tick does not repeat it.
    assert_eq!(r.refunds.len(), 1, "{:?}", r.round.failed);
    let refund_txid = r.refunds[0].2;
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert!(r.refunds.is_empty(), "pending refund is not repeated");
    mine(1);
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert!(r.refunds.is_empty(), "refund no longer due once its spend is seen");
    let spent = cluster.operators[1]
        .node
        .store
        .note_by_deposit_id(&voided)
        .unwrap()
        .unwrap()
        .spent
        .expect("operator 1's own scan saw the refund spend");
    assert_eq!(spent.0, refund_txid);
    for op in &cluster.operators {
        assert!(op.node.halted().unwrap().is_none());
        assert!(op.node.guard(&mut world.ledger).unwrap().is_empty(), "reconciles cleanly");
    }
    let _ = ChainEvent::Rewind { to_height: 0 };
    println!("ok: genesis, activation check, credit, void, FROST refund, spend detection");
}

#[test]
#[ignore]
fn phase4_payout_end_to_end_on_regtest() {
    let Setup { _procs, mut world, mut cluster, vault_addr } = setup();
    let h = node_height();
    let players = [SigningKey::from_bytes(&[0xC1; 32]), SigningKey::from_bytes(&[0xC2; 32])];
    let dests = [external_receiver(), external_receiver()];
    for (p, to) in players.iter().zip(&dests) {
        let m = session_memo(p, &vault_addr, h + 1_000, *to);
        let r = cluster.operators[0].zcashd.session_receiver(&app_id(), &m.session_key).unwrap();
        cluster.operators[0].zcashd.dev_send(&r.receiver, 1_000_000, &m.encode_text().unwrap().0).unwrap();
    }
    mine(10);
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert_eq!(r.round.credited.len(), 2, "{:?}", r.round);

    // Both players exit; anyone settles.
    let keys: Vec<solana_keypair::Keypair> = players.iter().map(|p| solana_keypair::Keypair::new_from_array(p.to_bytes())).collect();
    for k in &keys {
        ok!(world.ledger.env.request_exit(k));
    }
    let pubs: Vec<Pubkey> = keys.iter().map(solana_signer::Signer::pubkey).collect();
    ok!(world.ledger.env.settle(&pubs));
    let settlement: zol_ledger::state::Settlement = world.ledger.env.account(&settlement_pda(&app_pda(&TEST_APP), 0));

    // The quorum pays the record: proposal by the coordinator, verification and
    // a share from each operator's own single-share zcashd, broadcast.
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert_eq!(r.payouts.len(), 1, "waits={:?} failed={:?}", r.waits, r.round.failed);
    let payout_txid = r.payouts[0].1;
    // While it is pending nothing else is proposed.
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert!(r.payouts.is_empty());
    mine(1);
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert!(r.payouts.is_empty(), "nothing left to pay: {:?}", r.waits);
    for op in &cluster.operators {
        let head = op.node.head(&app_id()).unwrap();
        let last = head.chain.last().unwrap();
        assert_eq!((last.position, last.paid_count, last.txid), (1, 1, payout_txid), "{}", op.node.name);
        assert!(head.fault.is_none());
    }

    // After payout depth the quorum marks the settlement paid on the ledger.
    mine(100);
    sync(&cluster);
    let r = cluster.tick(&mut world.ledger, now(), &mut OsRng).unwrap();
    assert_eq!(r.round.marked_paid, vec![0], "{:?}", r.round);
    let s: zol_ledger::state::Settlement = world.ledger.env.account(&settlement_pda(&app_pda(&TEST_APP), 0));
    assert_eq!(s.state, zol_ledger::state::SettlementState::Paid);
    assert_eq!(world.ledger.env.app_state().unpaid_total, 0);
    for op in &cluster.operators {
        assert!(op.node.halted().unwrap().is_none(), "{}", op.node.name);
        assert!(op.node.guard(&mut world.ledger).unwrap().is_empty());
    }
    world.ledger.env.check_invariants();
    println!("ok: payout of {} zat in {} lines, head advanced, marked paid", settlement.total, settlement.count);
}
