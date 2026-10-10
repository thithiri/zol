//! Regtest helpers shared by the cross-process tests: Zebra RPC, zol-zcashd
//! processes in dev mode, test receivers and memos.
#![allow(dead_code)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer as _, SigningKey};
use rand::rngs::OsRng;
use serde_json::json;
use zol_gateway::zcashd::ZcashdClient;
use zol_wire::{DepositMemo, Network};

use super::app_id;

pub const DEV_SEED: &str = "zol-phase3-e2e";

pub struct Proc(pub Child, std::path::PathBuf);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

pub fn zebra_url() -> String {
    std::env::var("ZOL_ZEBRA_URL").unwrap_or_else(|_| "http://127.0.0.1:18232/".into())
}

pub fn zebra(method: &str, params: serde_json::Value) -> serde_json::Value {
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

pub fn zebra_height() -> u32 {
    zebra("getblockcount", json!([])).as_u64().unwrap() as u32
}

pub fn mine(n: u32) {
    zebra("generate", json!([n]));
}

pub fn zcashd_bin() -> std::path::PathBuf {
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
pub fn spawn_zcashd(i: usize, port: u16, start_height: u32) -> (Proc, ZcashdClient) {
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
pub fn wait_synced(clients: &[&ZcashdClient]) {
    let target = zebra_height();
    let start = Instant::now();
    for c in clients {
        while c.events(u64::MAX - 1, 1).map(|e| e.tip).unwrap_or(0) < target {
            assert!(start.elapsed() < Duration::from_secs(120), "zcashd did not sync to {target}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

pub fn session_memo(sk: &SigningKey, vault: &[u8; 43], expiry: u32, refund: [u8; 43]) -> DepositMemo {
    let mut m = DepositMemo::new(0, sk.verifying_key().to_bytes(), refund, expiry, [0; 32], b"e2e").unwrap();
    m.set_signature(sk.sign(&m.signing_message(Network::Regtest, vault)).to_bytes());
    m
}

/// An ordinary (non-vault) receiver: any diversifier and a valid Pallas point
/// for pk_d, which is all a sender needs.
pub fn external_receiver() -> [u8; 43] {
    use group::{Group, GroupEncoding};
    let mut r = [0u8; 43];
    r[..11].copy_from_slice(&rand::random::<[u8; 11]>());
    let p = pasta_curves::pallas::Point::random(&mut OsRng);
    r[11..].copy_from_slice(&p.to_bytes());
    r
}

