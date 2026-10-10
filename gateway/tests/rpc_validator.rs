//! The RPC relayer against a real `solana-test-validator` (ignored by default:
//! needs the validator binary and ~1 minute).
//!
//!     cargo test -p zol-gateway --test rpc_validator -- --ignored --nocapture
//!
//! `Config` and an already-active `App` are preloaded as accounts, since the
//! validator's clock cannot be moved past the one-hour activation delay.

mod common;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anchor_lang::prelude::Pubkey;
use anchor_lang::AccountSerialize;
use base64::Engine;
use common::*;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde_json::json;
use solana_keypair::Keypair;
use solana_signer::Signer;
use zol_gateway::attestor::{dkg, group_key};
use zol_gateway::chain::ChainEvent;
use zol_gateway::ledger::Ledger;
use zol_gateway::node::{coordinate, GuardAction};
use zol_gateway::rpc::RpcLedger;
use zol_ledger::state::{App, AppState, Config, DepositState};
use zol_tests::*;

struct Validator(Child, std::path::PathBuf);
impl Drop for Validator {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn dump(dir: &std::path::Path, name: &str, key: &Pubkey, data: Vec<u8>) -> String {
    let path = dir.join(format!("{name}.json"));
    let v = json!({
        "pubkey": key.to_string(),
        "account": {
            "lamports": 1_000_000_000u64,
            "data": [base64::engine::general_purpose::STANDARD.encode(&data), "base64"],
            "owner": LEDGER.to_string(),
            "executable": false,
            "rentEpoch": 0,
            "space": data.len()
        }
    });
    std::fs::write(&path, v.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

fn serialize<T: AccountSerialize>(a: &T, space: usize) -> Vec<u8> {
    let mut d = Vec::new();
    a.try_serialize(&mut d).unwrap();
    d.resize(space, 0);
    d
}

#[test]
#[ignore]
fn relayer_credits_voids_and_freezes_on_a_real_validator() {
    let (keys, pubkeys) = dkg(3, 2, &mut OsRng).unwrap();
    let guardians: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let admin = Keypair::new();
    let dir = std::env::temp_dir().join(format!("zol-rpc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut g = [Pubkey::default(); 7];
    for (i, k) in guardians.iter().enumerate() {
        g[i] = k.pubkey();
    }
    let config = Config {
        admin: admin.pubkey(),
        attestor: Pubkey::new_from_array(group_key(&pubkeys)),
        guardians: g,
        guardian_count: 3,
        guardian_threshold: 2,
        network: NETWORK,
        paused: false,
        params: params(),
        bump: Pubkey::find_program_address(&[b"config"], &LEDGER).1,
    };
    let app = App {
        program_id: TEST_APP,
        authority: app_authority(&TEST_APP),
        registrant: admin.pubkey(),
        state: AppState::Active,
        activates_at: 0,
        vault_address: VAULT,
        genesis_head: [0x9E; 32],
        deposit_cap: u64::MAX / 4,
        liability: 0,
        deposit_count: 0,
        credited_total: 0,
        fee_pool: 0,
        next_settlement: 0,
        paid_count: 0,
        unpaid_total: 0,
        bond: 0,
        bump: Pubkey::find_program_address(&[b"app", TEST_APP.as_ref()], &LEDGER).1,
    };
    use anchor_lang::Space;
    let cfg_path = dump(&dir, "config", &config_pda(), serialize(&config, 8 + Config::INIT_SPACE));
    let app_path = dump(&dir, "app", &app_pda(&TEST_APP), serialize(&app, 8 + App::INIT_SPACE));

    let port = 18_899 + (std::process::id() % 500) as u16;
    let so = deploy_dir().join("zol_ledger.so");
    let child = Command::new("solana-test-validator")
        .args(["--reset", "--quiet", "--ledger"])
        .arg(dir.join("ledger"))
        .args(["--rpc-port", &port.to_string(), "--faucet-port", &(port + 2_000).to_string()])
        .args(["--gossip-port", &(port + 3_000).to_string(), "--dynamic-port-range", &format!("{}-{}", port + 4_000, port + 4_100)])
        .args(["--bpf-program", &LEDGER.to_string()])
        .arg(&so)
        .args(["--account", &config_pda().to_string(), &cfg_path])
        .args(["--account", &app_pda(&TEST_APP).to_string(), &app_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("solana-test-validator on PATH");
    let _v = Validator(child, dir.clone());
    let url = format!("http://127.0.0.1:{port}");

    let relayer = Keypair::new();
    let mut ledger = RpcLedger::new(&url, relayer.insecure_clone(), group_key(&pubkeys), NETWORK);
    // Wait for the validator, then fund relayer, guardians and admin.
    let start = Instant::now();
    while ledger.call("getHealth", json!([])).is_err() {
        assert!(start.elapsed() < Duration::from_secs(60), "validator did not start");
        std::thread::sleep(Duration::from_millis(500));
    }
    for k in [&relayer, &admin].into_iter().chain(guardians.iter()) {
        let sig = ledger.call("requestAirdrop", json!([k.pubkey().to_string(), 10_000_000_000u64])).unwrap();
        let _ = sig;
    }
    std::thread::sleep(Duration::from_secs(2));
    // Wait until the preloaded accounts are visible at finalized.
    while ledger.app(&TEST_APP).ok().flatten().is_none() {
        assert!(start.elapsed() < Duration::from_secs(90), "accounts not finalized");
        std::thread::sleep(Duration::from_millis(500));
    }

    let seeds: Vec<[u8; 32]> = guardians.iter().map(|k| *k.secret_bytes()).collect();
    let mut nodes = make_nodes(keys, &pubkeys, &seeds);
    let feed = |nodes: &Vec<zol_gateway::node::Node>, ev: ChainEvent| {
        for n in nodes {
            n.apply(&ev, 0).unwrap();
        }
    };
    let mut height = 0u32;
    let mine = |nodes: &Vec<zol_gateway::node::Node>, k: u32, height: &mut u32| {
        for _ in 0..k {
            *height += 1;
            feed(nodes, ChainEvent::Block { height: *height, hash: block_hash(*height, 0) });
        }
    };
    mine(&nodes, 1, &mut height);
    let genesis = {
        let memo = zol_wire::HeadMemo::genesis(zol_wire::Network::Testnet, app_id()).encode();
        let mut n = deposit_note(&memo, 5_000_000, height, 1);
        n.scope = zol_gateway::chain::Scope::Internal;
        n
    };
    feed(&nodes, ChainEvent::Note(genesis));
    let good = memo(&SigningKey::from_bytes(&[1; 32]), 10_000, [7; 43]);
    let expired = memo(&SigningKey::from_bytes(&[2; 32]), 3, [8; 43]);
    let n1 = deposit_note(&good.encode_text().unwrap().0, DENOM, height, 2);
    let n2 = deposit_note(&expired.encode(), DENOM, height, 3);
    feed(&nodes, ChainEvent::Note(n1.clone()));
    feed(&nodes, ChainEvent::Note(n2.clone()));
    mine(&nodes, 10, &mut height);

    let mut session = 0;
    let round = coordinate(&mut nodes, 0, &pubkeys, 2, &mut ledger, &mut session, &mut OsRng).unwrap();
    println!("round: {round:?}");
    assert_eq!((round.credited.len(), round.voided.len()), (1, 1), "{round:?}");
    let s = zol_gateway::config::AppSecrets::derive(&MASTER, zol_wire::Network::Testnet, &app_id());
    assert_eq!(ledger.deposit(&s.deposit_id(&n1.cmx)).unwrap().unwrap().state, DepositState::Credited);
    assert_eq!(ledger.deposit(&s.deposit_id(&n2.cmx)).unwrap().unwrap().state, DepositState::Voided);
    assert_eq!(ledger.deposits(&TEST_APP).unwrap().len(), 2, "getProgramAccounts filter on app");
    let bal = ledger.balance(&TEST_APP, &Pubkey::new_from_array(good.session_key)).unwrap().unwrap();
    assert_eq!(bal.amount, DENOM - DENOM * 30 / 10_000);
    for n in &nodes {
        assert!(n.guard(&mut ledger).unwrap().is_empty());
    }

    // Forged credit through a rotated attestor key: guardians freeze over RPC.
    let rogue = SigningKey::from_bytes(&[0xBA; 32]);
    let rot = ledger_ix(
        zol_ledger::accounts::AdminOnly { admin: admin.pubkey(), config: config_pda() },
        zol_ledger::instruction::SetConfig {
            args: zol_ledger::SetConfigArgs {
                new_admin: None,
                attestor: Some(Pubkey::new_from_array(rogue.verifying_key().to_bytes())),
                params: None,
                paused: None,
            },
        },
    );
    ledger.send(&[rot], &[&admin]).unwrap();
    let mut rogue_ledger = RpcLedger::new(&url, relayer.insecure_clone(), rogue.verifying_key().to_bytes(), NETWORK);
    let args = zol_ledger::CreditArgs {
        deposit_id: [0xF0; 32],
        amount: 5 * DENOM,
        session_key: Pubkey::new_unique(),
        payout_commitment: [1; 32],
        flags: 0,
        pot_id: [0; 32],
        app_data: vec![],
    };
    let digest = Env::credit_digest(&args, &TEST_APP);
    use ed25519_dalek::Signer as _;
    rogue_ledger.submit_credit(&TEST_APP, args, rogue.sign(&digest).to_bytes()).unwrap();
    let actions = nodes[1].guard(&mut ledger).unwrap();
    assert!(matches!(&actions[..], [GuardAction::Froze { .. }]), "{actions:?}");
    assert_eq!(ledger.app(&TEST_APP).unwrap().unwrap().state, AppState::Frozen);
    println!("ok: credit, void, reconcile and guardian freeze over RPC");
}

fn deposit_note(memo: &[u8; 512], value: u64, height: u32, cmx: u8) -> zol_gateway::chain::ScannedNote {
    zol_gateway::chain::ScannedNote {
        app: app_id(),
        cmx: [cmx; 32],
        nullifier: [cmx ^ 0xAA; 32],
        value,
        memo: memo.to_vec(),
        receiver: VAULT.to_vec(),
        receiver_index: 1,
        scope: zol_gateway::chain::Scope::External,
        height,
        block_hash: block_hash(height, 0),
        txid: [cmx; 32],
    }
}
