//! Minimal operator wallet for funding (testnet float, dev deposits).
//!
//!   zol-wallet new --network testnet --out ../../keys/testnet-wallet.json
//!   zol-wallet address --key ../../keys/testnet-wallet.json
//!
//! The key file holds a raw Orchard spending key (hex). Keep it out of git.

use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use rand::rngs::OsRng;
use zol_zcash::keys::encode_ua;
use zol_zcash::params::ChainParams;

fn params(net: &str) -> ChainParams {
    match net {
        "mainnet" => ChainParams::MAINNET,
        "testnet" => ChainParams::TESTNET,
        "regtest" => ChainParams::REGTEST,
        other => panic!("unknown network {other}"),
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn show(sk: &SpendingKey, net: &str) {
    let fvk = FullViewingKey::from(sk);
    let addr = fvk.address_at(0u32, Scope::External).to_raw_address_bytes();
    println!("network: {net}");
    println!("unified address (Orchard receiver; receives Ironwood notes after NU6.3):");
    println!("{}", encode_ua(&params(net), &addr));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("new") => {
            let net = arg(&args, "--network").unwrap_or_else(|| "testnet".into());
            let out = arg(&args, "--out").expect("--out <file>");
            if std::path::Path::new(&out).exists() {
                panic!("{out} exists; refusing to overwrite a key");
            }
            let sk = zol_zcash::testing::random_spending_key(&mut OsRng);
            let json = serde_json::json!({ "network": net, "orchard_spending_key": hex::encode(sk.to_bytes()) });
            std::fs::write(&out, serde_json::to_string_pretty(&json).unwrap()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            show(&sk, &net);
        }
        Some("address") => {
            let key = arg(&args, "--key").expect("--key <file>");
            let (net, sk) = zol_zcash::keyfile::read_wallet_file(std::path::Path::new(&key))
                .unwrap_or_else(|e| panic!("{e}"));
            show(&sk, zol_zcash::keyfile::network_name(net));
        }
        _ => eprintln!("usage: zol-wallet new --network testnet --out <file> | zol-wallet address --key <file>"),
    }
}
