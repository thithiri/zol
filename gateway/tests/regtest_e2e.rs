//! Phase 3 end to end, across processes (ignored by default):
//!
//! Zebra regtest  ←→  3 × zol-zcashd (one per operator, own scan)  ←→  gateway
//! cluster (3 nodes, 2-of-3 FROST attestor)  ←→  ledger program on LiteSVM.
//!
//! Needs the regtest pair (`spikes/zebra/scripts/up.sh pair`) and a built
//! `zol-zcashd` (`cargo build --release --manifest-path crates/zol-zcash/Cargo.toml --bin zol-zcashd`):
//!
//!     cargo test -p zol-gateway --test regtest_e2e -- --ignored --nocapture --test-threads=1
//!
//! Env: ZOL_ZEBRA_URL (default http://127.0.0.1:18232/), ZOL_ZCASHD_BIN.

mod common;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anchor_lang::prelude::Pubkey;
use common::*;
use ed25519_dalek::{Signer as _, SigningKey};
use rand::rngs::OsRng;
use serde_json::json;
use zol_gateway::attestor::dkg;
use zol_gateway::chain::ChainEvent;
use zol_gateway::ledger::Ledger;
use zol_gateway::node::{AppRuntime, Node};
use zol_gateway::service::{Cluster, Operator};
use zol_gateway::store::Store;
use zol_gateway::zcashd::{OrchardReceivers, ZcashdClient};
use zol_ledger::state::{AppState, DepositState};
use zol_tests::*;
use zol_wire::{DepositMemo, Network};

const DEV_SEED: &str = "zol-phase3-e2e";

struct Proc(Child, std::path::PathBuf);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn zebra_url() -> String {
    std::env::var("ZOL_ZEBRA_URL").unwrap_or_else(|_| "http://127.0.0.1:18232/".into())
}

fn zebra(method: &str, params: serde_json::Value) -> serde_json::Value {
    let v: serde_json::Value = reqwest::blocking::Client::new()
        .post(zebra_url())
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(v["error"].is_null(), "{method}: {}", v["error"]);
    v["result"].clone()
}

fn zebra_height() -> u32 {
    zebra("getblockcount", json!([])).as_u64().unwrap() as u32
}

fn mine(n: u32) {
    zebra("generate", json!([n]));
}

fn zcashd_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("ZOL_ZCASHD_BIN") {
        return p.into();
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/zol-zcash/target");
    for p in ["release/zol-zcashd", "debug/zol-zcashd"] {
        if root.join(p).exists() {
            return root.join(p);
        }
    }
    panic!("build zol-zcashd first (see module docs)");
}

/// Writes a dev config and starts one zol-zcashd.
fn spawn_zcashd(i: usize, port: u16, start_height: u32) -> (Proc, ZcashdClient) {
    let dir = std::env::temp_dir().join(format!("zol-e2e-{}-{port}-{i}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = format!(
        "listen = \"127.0.0.1:{port}\"\nnetwork = \"regtest\"\nstate_dir = \"{}\"\nstart_height = {start_height}\npoll_ms = 300\napps = [\"{}\"]\n[zebra]\nurl = \"{}\"\n",
        dir.join("state").display(),
        hex::encode(app_id()),
        zebra_url()
    );
    let path = dir.join("zcashd.toml");
    std::fs::write(&path, cfg).unwrap();
    let child = Command::new(zcashd_bin())
        .args(["--config"])
        .arg(&path)
        .args(["--dev", "--dev-seed", DEV_SEED, "--dev-share", &i.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let client = ZcashdClient::new(&format!("http://127.0.0.1:{port}"));
    let start = Instant::now();
    while !client.health() {
        assert!(start.elapsed() < Duration::from_secs(60), "zcashd {i} did not start");
        std::thread::sleep(Duration::from_millis(300));
    }
    (Proc(child, dir), client)
}

/// Waits until every zcashd has scanned up to Zebra's tip.
fn wait_synced(clients: &[&ZcashdClient]) {
    let target = zebra_height();
    let start = Instant::now();
    for c in clients {
        while c.events(u64::MAX - 1, 1).map(|e| e.tip).unwrap_or(0) < target {
            assert!(start.elapsed() < Duration::from_secs(120), "zcashd did not sync to {target}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

fn session_memo(sk: &SigningKey, vault: &[u8; 43], expiry: u32, refund: [u8; 43]) -> DepositMemo {
    let mut m = DepositMemo::new(0, sk.verifying_key().to_bytes(), refund, expiry, [0; 32], b"e2e").unwrap();
    m.set_signature(sk.sign(&m.signing_message(Network::Regtest, vault)).to_bytes());
    m
}

/// An ordinary (non-vault) receiver: any diversifier and a valid Pallas point
/// for pk_d, which is all a sender needs.
fn external_receiver() -> [u8; 43] {
    use group::{Group, GroupEncoding};
    let mut r = [0u8; 43];
    r[..11].copy_from_slice(&rand::random::<[u8; 11]>());
    let p = pasta_curves::pallas::Point::random(&mut OsRng);
    r[11..].copy_from_slice(&p.to_bytes());
    r
}

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
    let start_height = zebra_height();
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 18_700 + (std::process::id() % 100) as u16 * 3 + 400 * NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let (p0, z0) = spawn_zcashd(0, base, start_height);
    let (p1, z1) = spawn_zcashd(1, base + 1, start_height);
    let (p2, z2) = spawn_zcashd(2, base + 2, start_height);
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
    cfg.denominations = vec![1_000_000, 10_000_000];
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
    let h = zebra_height();
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
    let h = zebra_height();
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
