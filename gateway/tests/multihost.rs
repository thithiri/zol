//! Three operators as separate gateways talking only over authenticated HTTP
//! (ignored by default). Each has its own single-share zol-zcashd, its own
//! store and its own Solana RPC connection to a real solana-test-validator.
//!
//! Needs the Zakura regtest (`infra/zakura/scripts/up.sh`), a built zol-zcashd, and
//! solana-test-validator (env as in regtest_e2e.rs):
//!
//!     cargo test -p zol-gateway --test multihost -- --ignored --nocapture

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AccountSerialize, Space};
use base64::Engine;
use common::regtest::*;
use common::*;
use ed25519_dalek::{Signer as _, SigningKey};
use rand::rngs::OsRng;
use serde_json::json;
use solana_keypair::Keypair;
use solana_signer::Signer;
use zol_gateway::attestor::{dkg, group_key};
use zol_gateway::ledger::Ledger;
use zol_gateway::rpc::RpcLedger;
use zol_gateway::serve::{self, AppCfg, PeerCfg, ServeConfig};
use zol_ledger::state::{App, AppState, Config, DepositState, SettlementState};
use zol_tests::*;
use zol_wire::Network;

struct Validator(std::process::Child, std::path::PathBuf);
impl Drop for Validator {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn dump(dir: &std::path::Path, name: &str, key: &Pubkey, data: Vec<u8>) -> String {
    let path = dir.join(format!("{name}.json"));
    let v = json!({"pubkey": key.to_string(), "account": {
        "lamports": 1_000_000_000u64,
        "data": [base64::engine::general_purpose::STANDARD.encode(&data), "base64"],
        "owner": LEDGER.to_string(), "executable": false, "rentEpoch": 0, "space": data.len()}});
    std::fs::write(&path, v.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

fn ser<T: AccountSerialize>(a: &T, space: usize) -> Vec<u8> {
    let mut d = Vec::new();
    a.try_serialize(&mut d).unwrap();
    d.resize(space, 0);
    d
}

fn write_keypair(dir: &std::path::Path, name: &str, k: &Keypair) -> std::path::PathBuf {
    let p = dir.join(format!("{name}.json"));
    std::fs::write(&p, serde_json::to_string(&k.to_bytes().to_vec()).unwrap()).unwrap();
    p
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore]
fn three_hosts_over_authenticated_http() {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("zol-multihost-{pid}"));
    std::fs::create_dir_all(&dir).unwrap();

    // Zcash side: three single-share daemons and a genesis head.
    let start_height = node_height();
    let base = 20_000 + (pid % 500) as u16 * 3;
    let (p0, z0) = spawn_zcashd(0, base, start_height);
    let (p1, z1) = spawn_zcashd(1, base + 1, start_height);
    let (p2, z2) = spawn_zcashd(2, base + 2, start_height);
    let _procs = (p0, p1, p2);
    let vault = z0.vault(&app_id()).unwrap();
    let vault_addr: [u8; 43] = vault.address.as_slice().try_into().unwrap();
    let g = z0.dev_genesis(&app_id(), 30_000_000).unwrap();
    mine(101);
    wait_synced(&[&z0, &z1, &z2]);

    // Keys: DKG attestor, guardians, relayers, admin.
    let (keys, pubkeys) = dkg(3, 2, &mut OsRng).unwrap();
    std::fs::write(dir.join("pubkeys.json"), serde_json::to_string(&pubkeys).unwrap()).unwrap();
    for (i, k) in keys.iter().enumerate() {
        std::fs::write(dir.join(format!("share{i}.json")), serde_json::to_string(k).unwrap()).unwrap();
    }
    let guardians: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let relayers: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let admin = Keypair::new();

    // Solana: a real validator with Config and an active App preloaded.
    let mut gk = [Pubkey::default(); 7];
    for (i, k) in guardians.iter().enumerate() {
        gk[i] = k.pubkey();
    }
    let mut p = params();
    p.min_exit = 100_000;
    // Covers the ZIP 317 fee of the smallest padded transition (5,000 × 4 actions).
    p.network_fee_allowance = 20_000;
    let config = Config {
        admin: admin.pubkey(),
        attestor: Pubkey::new_from_array(group_key(&pubkeys)),
        guardians: gk,
        guardian_count: 3,
        guardian_threshold: 2,
        network: Network::Regtest as u8,
        paused: false,
        params: p,
        bump: Pubkey::find_program_address(&[b"config"], &LEDGER).1,
    };
    let app = App {
        program_id: TEST_APP,
        authority: app_authority(&TEST_APP),
        registrant: admin.pubkey(),
        state: AppState::Active,
        activates_at: 0,
        vault_address: vault_addr,
        genesis_head: g.head_cmx,
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
    let cfg_path = dump(&dir, "config", &config_pda(), ser(&config, 8 + Config::INIT_SPACE));
    let app_path = dump(&dir, "app", &app_pda(&TEST_APP), ser(&app, 8 + App::INIT_SPACE));
    let rpc_port = 19_899 + (pid % 300) as u16;
    let child = Command::new("solana-test-validator")
        .args(["--reset", "--quiet", "--ledger"])
        .arg(dir.join("ledger"))
        .args(["--rpc-port", &rpc_port.to_string(), "--faucet-port", &(rpc_port + 2_000).to_string()])
        .args(["--gossip-port", &(rpc_port + 3_000).to_string(), "--dynamic-port-range", &format!("{}-{}", rpc_port + 4_000, rpc_port + 4_100)])
        .args(["--bpf-program", &LEDGER.to_string()])
        .arg(deploy_dir().join("zol_ledger.so"))
        .args(["--account", &config_pda().to_string(), &cfg_path])
        .args(["--account", &app_pda(&TEST_APP).to_string(), &app_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("solana-test-validator");
    let _validator = Validator(child, dir.join("ledger"));
    let url = format!("http://127.0.0.1:{rpc_port}");
    let mut ledger = RpcLedger::new(&url, Keypair::new(), group_key(&pubkeys), Network::Regtest as u8);
    ledger.send_commitment = "confirmed";
    wait_until("validator", 60, || ledger.call("getHealth", json!([])).is_ok());
    for k in relayers.iter().chain(guardians.iter()).chain([&admin]) {
        let _ = ledger.call("requestAirdrop", json!([k.pubkey().to_string(), 10_000_000_000u64]));
    }
    let funder = Keypair::new();
    let _ = ledger.call("requestAirdrop", json!([funder.pubkey().to_string(), 10_000_000_000u64]));
    wait_until("finalized accounts", 90, || ledger.app(&TEST_APP).ok().flatten().is_some());
    let mut ledger = RpcLedger::new(&url, funder, group_key(&pubkeys), Network::Regtest as u8);
    ledger.send_commitment = "confirmed";

    // Three gateways, each knowing the other two by URL and guardian key.
    let names = ["op0", "op1", "op2"];
    let gw_port = base + 900;
    let zurls = [base, base + 1, base + 2].map(|p| format!("http://127.0.0.1:{p}"));
    let mut gcfg = gw_config();
    gcfg.network = Network::Regtest as u8;
    gcfg.early_credit_cap = 10_000_000;
    let mut handles = Vec::new();
    for i in 0..3 {
        let cfg = ServeConfig {
            gateway: gcfg.clone(),
            name: names[i].into(),
            listen: format!("127.0.0.1:{}", gw_port + i as u16),
            public_url: Some(format!("http://127.0.0.1:{}", gw_port + i as u16)),
            solana_rpc: url.clone(),
            relayer_keypair: write_keypair(&dir, &format!("relayer{i}"), &relayers[i]),
            store: dir.join(format!("store{i}.db")),
            zcashd: zurls[i].clone(),
            guardian_keypair: write_keypair(&dir, &format!("guardian{i}"), &guardians[i]),
            attestor_share: dir.join(format!("share{i}.json")),
            attestor_pubkeys: dir.join("pubkeys.json"),
            threshold: 2,
            apps: vec![AppCfg { program: TEST_APP.to_string(), gateway_master_secret: hex::encode(MASTER) }],
            peers: (0..3)
                .filter(|j| *j != i)
                .map(|j| PeerCfg {
                    name: names[j].into(),
                    url: format!("http://127.0.0.1:{}", gw_port + j as u16),
                    guardian: guardians[j].pubkey().to_string(),
                    public_url: None,
                })
                .collect(),
            order: names.iter().map(|s| s.to_string()).collect(),
            epoch_secs: 8,
            poll_ms: 400,
            send_commitment: "confirmed".into(),
        };
        handles.push(Some(serve::start(cfg).unwrap()));
    }
    let gw = |i: usize| format!("http://127.0.0.1:{}", gw_port + i as u16);
    let http = reqwest::blocking::Client::new();

    // Page API: app info, intent registration, session receiver.
    let info: zol_types::page::AppInfo = http.get(format!("{}/v1/apps/{}", gw(1), TEST_APP)).send().unwrap().json().unwrap();
    assert_eq!((info.state.as_str(), info.network.as_str()), ("active", "regtest"));
    assert_eq!(info.vault_address, hex::encode(vault_addr));
    assert_eq!(info.operators, (0..3).map(gw).collect::<Vec<_>>());
    let h = node_height();
    let alice = SigningKey::from_bytes(&[0xA1; 32]);
    let bob = SigningKey::from_bytes(&[0xB0; 32]);
    let ma = session_memo(&alice, &vault_addr, h + 1_000, external_receiver());
    let mb = session_memo(&bob, &vault_addr, h + 3, external_receiver());
    // Registered with one operator; gossip carries it to the other two.
    let s: zol_types::page::SessionInfo = http
        .post(format!("{}/v1/apps/{}/intents", gw(0), TEST_APP))
        .json(&zol_types::page::Intent { memo: hex::encode(ma.encode()) })
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(s.intent_registered);
    for i in 1..3 {
        wait_until("intent gossip", 30, || {
            http.get(format!("{}/v1/apps/{}/sessions/{}", gw(i), TEST_APP, hex::encode(ma.session_key)))
                .send()
                .ok()
                .and_then(|r| r.json::<zol_types::page::SessionInfo>().ok())
                .is_some_and(|s| s.intent_registered)
        });
    }
    let ra = z0.session_receiver(&app_id(), &ma.session_key).unwrap();
    let rb = z0.session_receiver(&app_id(), &mb.session_key).unwrap();
    // Alice's wallet drops the memo; the registered intent still credits her.
    let mut empty = vec![0u8; 512];
    empty[0] = 0xF6;
    z0.dev_send(&ra.receiver, 1_000_000, &empty).unwrap();
    z0.dev_send(&rb.receiver, 1_000_000, &mb.encode()).unwrap();
    mine(10);

    let s = zol_gateway::config::AppSecrets::derive(&MASTER, Network::Regtest, &app_id());
    let _ = &s;
    wait_until("credit and void relayed by a coordinator", 120, || {
        let deps = ledger.deposits(&TEST_APP).unwrap_or_default();
        deps.iter().any(|(_, d)| d.state == DepositState::Credited) && deps.iter().any(|(_, d)| d.state == DepositState::Voided)
    });
    let bal = ledger.balance(&TEST_APP, &Pubkey::new_from_array(ma.session_key)).unwrap().unwrap();
    assert_eq!(bal.amount, 1_000_000 - 1_000_000 * 30 / 10_000);

    // Bob's refund: distributed spend signing over HTTP, then mined and seen.
    let bob_hex = hex::encode(mb.session_key);
    wait_until("bob refunded", 180, || {
        mine(1);
        std::thread::sleep(Duration::from_millis(800));
        let deps: Vec<zol_types::page::DepositStatus> = http
            .get(format!("{}/v1/apps/{}/sessions/{bob_hex}/deposits", gw(2), TEST_APP))
            .send()
            .and_then(|r| r.json())
            .unwrap_or_default();
        deps.iter().any(|d| d.state == "refunded")
    });

    // Peer authentication.
    let peer_url = format!("{}/peer/chain_report", gw(0));
    assert_eq!(http.post(&peer_url).body("null").send().unwrap().status().as_u16(), 401, "no signature");
    let signed = |key: &SigningKey, ts: i64, nonce: &str| {
        let sig = key.sign(&zol_gateway::peer::signing_message("/peer/chain_report", ts, nonce, b"null"));
        http.post(&peer_url)
            .header("x-zol-peer", hex::encode(key.verifying_key().to_bytes()))
            .header("x-zol-ts", ts.to_string())
            .header("x-zol-nonce", nonce)
            .header("x-zol-sig", hex::encode(sig.to_bytes()))
            .body("null")
    };
    let n1 = hex::encode([1u8; 16]);
    let n2 = hex::encode([2u8; 16]);
    let stranger = SigningKey::from_bytes(&[0x55; 32]);
    assert_eq!(signed(&stranger, zol_gateway::peer::now_secs(), &n1).send().unwrap().status().as_u16(), 401, "unknown key");
    let g1 = SigningKey::from_bytes(guardians[1].secret_bytes());
    let ts = zol_gateway::peer::now_secs();
    let r = signed(&g1, ts, &n1).send().unwrap();
    let code = r.status().as_u16();
    assert_eq!(code, 200, "listed peer: {}", r.text().unwrap_or_default());
    assert_eq!(signed(&g1, ts, &n1).send().unwrap().status().as_u16(), 401, "replay");
    assert_eq!(signed(&g1, ts, &n2).send().unwrap().status().as_u16(), 200, "same second, fresh nonce");
    assert_eq!(signed(&g1, ts - 600, &hex::encode([3u8; 16])).send().unwrap().status().as_u16(), 401, "stale");

    // One operator goes down; the other two still pay out.
    handles[2].take().unwrap().shutdown();
    let alice_kp = Keypair::new_from_array(alice.to_bytes());
    let app = app_pda(&TEST_APP);
    let exit = ledger_ix(
        zol_ledger::accounts::RequestExit { session_key: alice_kp.pubkey(), config: config_pda(), balance: balance_pda(&app, &alice_kp.pubkey()) },
        zol_ledger::instruction::RequestExit {},
    );
    ledger.send(&[exit], &[&alice_kp]).unwrap();
    let mut settle = ledger_ix(
        zol_ledger::accounts::Settle {
            payer: solana_signer::Signer::pubkey(&Keypair::new()),
            config: config_pda(),
            app,
            settlement: settlement_pda(&app, 0),
            system_program: SYSTEM,
        },
        zol_ledger::instruction::Settle {},
    );
    settle.accounts[0] = solana_instruction::AccountMeta::new(ledger_payer(&ledger), true);
    settle.accounts.push(solana_instruction::AccountMeta::new(balance_pda(&app, &alice_kp.pubkey()), false));
    ledger.send(&[settle], &[]).unwrap();

    wait_until("payout mined", 240, || {
        mine(1);
        std::thread::sleep(Duration::from_millis(800));
        z0.head(&app_id(), &g.head_cmx).map(|h| h.chain.last().map(|l| l.paid_count) == Some(1)).unwrap_or(false)
    });
    mine(100);
    wait_until("settlement marked paid", 180, || {
        ledger.settlements(&TEST_APP).unwrap_or_default().iter().any(|s| s.sequence == 0 && s.state == SettlementState::Paid)
    });
    for h in handles.into_iter().flatten() {
        let halted = h.local.0.lock().unwrap().node.halted().unwrap();
        assert!(halted.is_none(), "{halted:?}");
        h.shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!("ok: 3 hosts over HTTP: intent credit, void, refund, auth, payout and mark_paid with one operator down");
}

fn ledger_payer(l: &RpcLedger) -> Pubkey {
    l.relayer_pubkey()
}
