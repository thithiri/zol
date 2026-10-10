//! `zol-zcashd keygen` and the `[funding]` startup checks, offline: runs the binary.

use std::{collections::BTreeMap, path::Path, process::Command};

use ed25519_dalek::{Signer as _, SigningKey};
use orchard::keys::{FullViewingKey, Scope};
use zol_wire::{DepositMemo, Network};
use zol_zcash::{
    block::ParsedBlock,
    frost, keyfile, refund,
    scan::{Account, ScannerConfig},
    testing, tx, AppKeys, ChainParams, Scanner,
};

const BIN: &str = env!("CARGO_BIN_EXE_zol-zcashd");
const P: ChainParams = ChainParams::REGTEST;

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let mut r = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut r);
    let d = std::env::temp_dir().join(format!("zol-keygen-{tag}-{}", hex::encode(r)));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn keygen(out: &Path) -> std::process::Output {
    Command::new(BIN)
        .args([
            "keygen",
            "--network",
            "regtest",
            "--operators",
            "3",
            "--threshold",
            "2",
        ])
        .arg("--out")
        .arg(out)
        .args(["--app", &hex::encode([0xA1u8; 32])])
        .output()
        .unwrap()
}

fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn block(height: u32, prev: [u8; 32], raws: &[&[u8]]) -> ParsedBlock {
    let mut hash = [0u8; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    ParsedBlock {
        height,
        hash,
        prev_hash: prev,
        txs: raws.iter().map(|r| tx::parse_tx(r).unwrap()).collect(),
    }
}

#[test]
fn keygen_writes_operator_files_that_sign_2_of_3() {
    let root = tmpdir("a");
    let out = root.join("keys");
    let o = keygen(&out);
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    println!("{stdout}");

    // Files: 0600, the directory 0700; group.toml holds no secrets.
    let files: Vec<_> = (1..=3)
        .map(|i| out.join(format!("operator-{i}.keys.toml")))
        .collect();
    for f in files.iter().chain([&out.join("group.toml")]) {
        assert_eq!(mode(f), 0o600, "{}", f.display());
    }
    assert_eq!(mode(&out), 0o700);
    let group: toml::Value =
        toml::from_str(&std::fs::read_to_string(out.join("group.toml")).unwrap()).unwrap();
    assert!(group.get("shares").is_none() && group.get("master_view").is_none());
    assert!(group.get("keys").is_none());
    assert_eq!(group["threshold"].as_integer(), Some(2));
    assert_eq!(group["operators"].as_integer(), Some(3));

    // Each operator file loads (as zol-zcashd does) with exactly one share: operator mode.
    let loaded: Vec<keyfile::Keys> = files
        .iter()
        .map(|f| keyfile::load(&keyfile::read_keys_file(f).unwrap()).unwrap())
        .collect();
    let pk = loaded[0].pubkeys.clone();
    let k_master = loaded[0].master_view;
    let mut shares = BTreeMap::new();
    for k in &loaded {
        assert_eq!(k.shares.len(), 1);
        assert!((k.shares.len() as u16) < k.pubkeys.min_signers().unwrap());
        assert_eq!(k.pubkeys, pk);
        assert_eq!(k.master_view, k_master);
        let (id, kp) = k.shares.iter().next().unwrap();
        assert!(
            shares.insert(*id, kp.clone()).is_none(),
            "distinct identifiers"
        );
    }
    let ak_group = frost::group_key(&pk);
    assert_eq!(ak_group[31] >> 7, 0, "ỹ(ak_group) = 0");
    assert_eq!(
        group["ak_group"].as_str(),
        Some(hex::encode(ak_group).as_str())
    );
    assert_eq!(
        group["master_view_fingerprint"].as_str(),
        Some(hex::encode(keyfile::master_view_fingerprint(&k_master)).as_str())
    );
    assert!(stdout.contains(&hex::encode(ak_group)));
    let app = AppKeys::derive(ak_group, &k_master, &[0xA1; 32], Network::Regtest).unwrap();
    assert!(stdout.contains(&zol_zcash::encode_ua(&P, &app.vault_address())));

    // Never overwrites: a second run into the same directory fails and changes nothing.
    let before = std::fs::read(&files[0]).unwrap();
    let o2 = keygen(&out);
    assert!(!o2.status.success());
    assert!(String::from_utf8_lossy(&o2.stderr).contains("refusing to overwrite"));
    assert_eq!(std::fs::read(&files[0]).unwrap(), before);

    // Fresh randomness: another run gives another group and master view.
    let out_b = root.join("keys-b");
    assert!(keygen(&out_b).status.success());
    let b = keyfile::load(&keyfile::read_keys_file(&out_b.join("operator-1.keys.toml")).unwrap())
        .unwrap();
    assert_ne!(frost::group_key(&b.pubkeys), ak_group);
    assert_ne!(b.master_view, k_master);

    // A real vault spend: a deposit to the app's vault, refunded with every pair of shares.
    let player_sk = testing::random_spending_key(&mut rand::rngs::OsRng);
    let player = FullViewingKey::from(&player_sk);
    let (w0, w0_path, w0_anchor) = testing::offline_note(&player_sk, 50_000_000);
    let refund_to = player
        .address_at(0u32, Scope::External)
        .to_raw_address_bytes();
    let sess = SigningKey::generate(&mut rand::rngs::OsRng);
    let (recv, _) = app.session_receiver(&sess.verifying_key().to_bytes());
    let mut m = DepositMemo::new(
        0,
        sess.verifying_key().to_bytes(),
        refund_to,
        1,
        [0; 32],
        b"k",
    )
    .unwrap();
    m.set_signature(
        sess.sign(&m.signing_message(Network::Regtest, &app.vault_address()))
            .to_bytes(),
    );
    let (dep_raw, _) = testing::wallet_send(
        &P,
        &player_sk,
        &w0,
        &w0_path,
        w0_anchor,
        &recv,
        10_000_000,
        m.encode_text().unwrap().0,
        1,
    )
    .unwrap();
    let mut sc = Scanner::new(
        ScannerConfig::new(P),
        vec![Account::from(&app)],
        1,
        Some([0; 32]),
        None,
    )
    .unwrap();
    let ev = sc.ingest_parsed(&block(1, [0; 32], &[&dep_raw])).unwrap();
    let dep = ev.received[0].clone();
    let sp = sc.spendable(&dep.cmx, None).unwrap();
    let unsigned = refund::build(&P, &app, &sp, &refund_to, 2, 20).unwrap();
    let ids: Vec<_> = shares.keys().copied().collect();
    for (a, b) in [(0, 1), (0, 2), (1, 2)] {
        let pair: BTreeMap<_, _> = [ids[a], ids[b]]
            .iter()
            .map(|i| (*i, shares[i].clone()))
            .collect();
        let r = refund::sign_with_frost(
            unsigned.clone(),
            &app,
            &sp.note,
            &pair,
            &pk,
            1,
            &mut rand::rngs::OsRng,
        )
        .unwrap_or_else(|e| panic!("pair {a},{b}: {e}"));
        let (raw, _) = refund::extract(r, &P).unwrap();
        // The signed spend is accepted by the scanner's parser and spends the deposit.
        assert!(zol_zcash::verify::nullifiers(&raw)
            .unwrap()
            .iter()
            .any(|x| x.1 == dep.nullifier));
    }
    // One share alone cannot sign.
    let one: BTreeMap<_, _> = [(ids[0], shares[&ids[0]].clone())].into();
    assert!(refund::sign_with_frost(
        unsigned,
        &app,
        &sp.note,
        &one,
        &pk,
        1,
        &mut rand::rngs::OsRng
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&root);
}

fn run_config(dir: &Path, body: &str, extra: &[&str]) -> std::process::Output {
    let cfg = dir.join("zcashd.toml");
    std::fs::write(&cfg, body).unwrap();
    Command::new(BIN)
        .arg("--config")
        .arg(&cfg)
        .args(extra)
        .output()
        .unwrap()
}

/// `[funding]` refuses a non-loopback listen address (before touching the node), and a
/// wallet file of another network.
#[test]
fn funding_needs_loopback_listen_and_matching_network() {
    let dir = tmpdir("funding");
    let wallet = dir.join("wallet.json");
    let sk = testing::random_spending_key(&mut rand::rngs::OsRng);
    let w = |net: &str| {
        let _ = std::fs::remove_file(&wallet);
        std::fs::write(
            &wallet,
            serde_json::json!({"network": net, "orchard_spending_key": hex::encode(sk.to_bytes())})
                .to_string(),
        )
        .unwrap();
    };
    w("testnet");
    let cfg = |listen: &str| {
        format!(
            r#"listen = "{listen}"
network = "testnet"
state_dir = "{}"
apps = []
[node]
url = "http://127.0.0.1:1/"
[funding]
key_file = "{}"
start_height = 4474000
"#,
            dir.join("state").display(),
            wallet.display()
        )
    };
    let o = run_config(&dir, &cfg("0.0.0.0:18300"), &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("not a loopback"),
        "{err}"
    );
    // With the override, it gets past the check (and fails later: no keys).
    let o = run_config(&dir, &cfg("0.0.0.0:18300"), &["--allow-remote-admin"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("`keys`"), "{err}");
    let o = run_config(&dir, &cfg("127.0.0.1:18300"), &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("`keys`"), "{err}");
    w("mainnet");
    let o = run_config(&dir, &cfg("127.0.0.1:18300"), &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("wallet network mainnet"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
