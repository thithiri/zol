//! JSON-RPC helpers shared by the e2e binaries (copied from main.rs, made public).
use base64::Engine;
use serde_json::{json, Value};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use std::time::{Duration, Instant};
use anchor_lang::AccountDeserialize;
pub const BASE: &str = "http://127.0.0.1:28899";
pub const ER: &str = "http://127.0.0.1:27799";
pub fn rpc(url: &str, method: &str, params: Value) -> Value {
    for attempt in 0..5 {
        match ureq::post(url).send_json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})) {
            Ok(r) => {
                let v: Value = r.into_json().unwrap();
                if let Some(e) = v.get("error") {
                    panic!("{method} error: {e}");
                }
                return v["result"].clone();
            }
            Err(e) if attempt < 4 => {
                eprintln!("rpc {method} retry: {e}");
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => panic!("{method}: {e}"),
        }
    }
    unreachable!()
}

pub fn blockhash(url: &str) -> solana_hash::Hash {
    let v = rpc(url, "getLatestBlockhash", json!([{"commitment":"confirmed"}]));
    v["value"]["blockhash"].as_str().unwrap().parse().unwrap()
}

pub struct Sent {
    pub sig: String,
    pub ms: u128,
}

#[derive(Debug)]
pub struct Failed {
    pub sig: String,
    pub err: Value,
    pub logs: Vec<String>,
}

pub fn tx_logs(url: &str, sig: &str) -> Vec<String> {
    for _ in 0..20 {
        let v = rpc(url, "getTransaction", json!([sig, {"encoding":"json","commitment":"confirmed","maxSupportedTransactionVersion":0}]));
        if !v.is_null() {
            return v["meta"]["logMessages"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect()).unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    vec![]
}

pub fn send(url: &str, ixs: &[Instruction], signers: &[&Keypair]) -> Result<Sent, Failed> {
    let payer = signers[0].pubkey();
    let msg = Message::new(ixs, Some(&payer));
    let tx = Transaction::new(&signers.to_vec(), msg, blockhash(url));
    let raw = base64::engine::general_purpose::STANDARD.encode(bincode::serialize(&tx).unwrap());
    let t0 = Instant::now();
    let res = ureq::post(url)
        .send_json(json!({"jsonrpc":"2.0","id":1,"method":"sendTransaction","params":[raw, {"encoding":"base64","skipPreflight":true}]}))
        .unwrap()
        .into_json::<Value>()
        .unwrap();
    if let Some(e) = res.get("error") {
        return Err(Failed { sig: String::new(), err: e.clone(), logs: vec![] });
    }
    let sig = res["result"].as_str().unwrap().to_string();
    loop {
        let st = rpc(url, "getSignatureStatuses", json!([[sig], {"searchTransactionHistory": false}]));
        let s = &st["value"][0];
        if !s.is_null() {
            if !s["err"].is_null() {
                let logs = tx_logs(url, &sig);
                return Err(Failed { sig, err: s["err"].clone(), logs });
            }
            let c = s["confirmationStatus"].as_str().unwrap_or("");
            if c == "confirmed" || c == "finalized" {
                return Ok(Sent { sig, ms: t0.elapsed().as_millis() });
            }
        }
        if t0.elapsed() > Duration::from_secs(60) {
            return Err(Failed { sig, err: json!("timeout"), logs: vec![] });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn account(url: &str, key: &Pubkey) -> Option<(Pubkey, Vec<u8>, u64)> {
    let v = rpc(url, "getAccountInfo", json!([key.to_string(), {"encoding":"base64","commitment":"confirmed"}]));
    let a = &v["value"];
    if a.is_null() {
        return None;
    }
    let data = base64::engine::general_purpose::STANDARD.decode(a["data"][0].as_str().unwrap()).unwrap();
    Some((a["owner"].as_str().unwrap().parse().unwrap(), data, a["lamports"].as_u64().unwrap()))
}

pub fn load<T: AccountDeserialize>(url: &str, key: &Pubkey) -> T {
    let (_, data, _) = account(url, key).unwrap_or_else(|| panic!("missing {key}"));
    T::try_deserialize(&mut &data[..]).unwrap()
}

pub fn now_on_chain(url: &str) -> i64 {
    let (_, d, _) = account(url, &solana_pubkey::pubkey!("SysvarC1ock11111111111111111111111111111111")).unwrap();
    i64::from_le_bytes(d[32..40].try_into().unwrap())
}

pub fn airdrop(key: &Pubkey, lamports: u64) {
    let sig = rpc(BASE, "requestAirdrop", json!([key.to_string(), lamports]));
    let sig = sig.as_str().unwrap().to_string();
    for _ in 0..100 {
        let st = rpc(BASE, "getSignatureStatuses", json!([[sig]]));
        if st["value"][0]["confirmationStatus"].as_str() == Some("confirmed") || st["value"][0]["confirmationStatus"].as_str() == Some("finalized") {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("airdrop not confirmed");
}
