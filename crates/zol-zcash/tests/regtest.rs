//! Regtest integration tests against Zebra 6.4.2 or Zebra 7 / NU7 (`spikes/zebra`). Ignored by
//! default:
//!
//! ```sh
//! (cd spikes/zebra && ./scripts/up.sh pair)
//! cd crates/zol-zcash && cargo test --test regtest -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Env: `ZOL_ZEBRA_URL` (default http://127.0.0.1:18232/), `ZOL_ZEBRA_B_URL`
//! (default http://127.0.0.1:18242/, reorg test only), `ZOL_DEV_SEED` (optional: use the
//! dev wallet of that seed, `testing::dev_seed_keys`, instead of a random one).
//! Reorg tests drive the node pair with `docker exec`/`spikes/zebra/scripts`; `ZOL_ZEBRA=zebra7`
//! selects the Zebra 7 NU7 pair (containers zol-zebra7-a/b, RPC 18272/18282).
//!
//! Zakura (no `generatetoaddress`; docs/ops/zcash-node.md): run its regtest node with
//! `[mining] miner_address` = `testing::dev_wallet_ua(REGTEST, seed)` and set
//! `ZOL_DEV_SEED=seed`; `fund` then falls back to `generate`.
//!
//! Funding: `generatetoaddress` with a Unified Address holding the test wallet's Orchard
//! receiver. Zebra 6.4.2 routes shielded coinbase to the Ironwood pool from NU6.3
//! (zebra-rpc `TransactionTemplate::new_coinbase`), so the wallet receives an Ironwood note.

use std::{collections::BTreeMap, process::Command, time::Duration};

use ed25519_dalek::{Signer as _, SigningKey};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use zol_wire::{DepositMemo, HeadMemo, Network};
use zol_zcash::{
    display_hex, encode_ua, frost, genesis, refund,
    scan::{Account, ScannedNote, ScannerConfig},
    testing,
    zebra::ZebraClient,
    AppKeys, ChainParams, Scanner,
};

/// Regtest parameters as the node reports them (set in `setup`): NU7 at 1 on the Zebra 7
/// regtest (`spikes/zebra/regtest-nu7.toml`), none on the 6.4.2 one.
static PARAMS: std::sync::OnceLock<ChainParams> = std::sync::OnceLock::new();
#[allow(non_snake_case)]
fn P() -> ChainParams {
    *PARAMS
        .get()
        .expect("setup() reads the regtest heights first")
}
const WALLET: [u8; 32] = [0xEE; 32];

fn url(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

fn script(name: &str) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spikes/zebra/scripts/");
    let out = Command::new("bash")
        .arg(format!("{path}{name}"))
        .output()
        .expect("run script");
    println!(
        "[{name}] {}{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

struct Env {
    client: ZebraClient,
    sk: SpendingKey,
    fvk: FullViewingKey,
    scanner: Scanner,
    app: AppKeys,
    kps: frost::KeyPackages,
    pk: frost::PublicKeyPackage,
}

async fn setup(client: ZebraClient) -> Env {
    let info = client
        .get_blockchain_info()
        .await
        .expect("zebra reachable (spikes/zebra/scripts/up.sh)");
    println!(
        "zebra: chain={} blocks={} chaintip branch={:?}",
        info.chain,
        info.blocks,
        info.chaintip_branch()
    );
    let params = ChainParams::regtest_from_upgrades(&info.rest["upgrades"]).unwrap();
    let _ = PARAMS.set(params);
    let tip = info.blocks + 1;
    let want = format!("{:08x}", u32::from(P().branch_id(info.blocks.max(1))));
    println!("regtest heights {:?}: tip branch {want}", P().regtest);
    // NU6.3 (37a5165b) on the 6.4.2 regtest, NU7 (77190ad9) on the Zebra 7 one.
    if info.blocks > 0 {
        assert_eq!(info.chaintip_branch().as_deref(), Some(want.as_str()));
    }
    assert!(P().ironwood_active(tip), "NU6.3 active");
    let mut rng = rand::rngs::OsRng;
    let (kps, pk) = frost::dkg_local(3, 2, &mut rng).unwrap();
    let app = AppKeys::derive(
        frost::group_key(&pk),
        &[0x42; 32],
        &rand_app(),
        Network::Regtest,
    )
    .unwrap();
    let sk = match std::env::var("ZOL_DEV_SEED") {
        Ok(seed) => testing::dev_seed_keys(&seed).wallet,
        Err(_) => testing::random_spending_key(&mut rng),
    };
    let fvk = FullViewingKey::from(&sk);
    let start = client.get_block_count().await.unwrap() + 1;
    let scanner = Scanner::from_zebra(
        ScannerConfig::new(P()),
        vec![
            Account::from(&app),
            Account {
                id: WALLET,
                fvk: fvk.clone(),
            },
        ],
        &client,
        start,
    )
    .await
    .unwrap();
    println!(
        "scanner starts at {start} (tree from z_gettreestate {})",
        start - 1
    );
    Env {
        client,
        sk,
        fvk,
        scanner,
        app,
        kps,
        pk,
    }
}

fn rand_app() -> [u8; 32] {
    let mut a = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut a);
    a
}

impl Env {
    async fn sync(&mut self) -> zol_zcash::scan::SyncReport {
        let r = self.scanner.sync(&self.client, None).await.unwrap();
        r
    }

    /// Mines a shielded coinbase to the wallet and returns the scanned note.
    async fn fund(&mut self) -> ScannedNote {
        let ua = encode_ua(
            &P(),
            &self
                .fvk
                .address_at(0u32, Scope::External)
                .to_raw_address_bytes(),
        );
        let t = std::time::Instant::now();
        let (hashes, fallback) = self
            .client
            .mine_to(1, &ua)
            .await
            .expect("generatetoaddress to a UA");
        println!(
            "{} {ua} -> {} ({:?})",
            if fallback {
                "generate (no generatetoaddress; miner_address must be)"
            } else {
                "generatetoaddress"
            },
            hashes[0],
            t.elapsed()
        );
        let rep = self.sync().await;
        let n = rep
            .events
            .iter()
            .flat_map(|e| e.received.iter())
            .find(|n| n.app_id == WALLET)
            .expect("coinbase Ironwood note scanned")
            .clone();
        println!(
            "funded: {} zat Ironwood coinbase note at height {}",
            n.value, n.height
        );
        assert!(
            n.value >= 1_000_000,
            "coinbase is only {} zat: the regtest subsidy is nearly exhausted; restart the chain (spikes/zebra/scripts/down.sh && up.sh pair)",
            n.value
        );
        n
    }

    /// Broadcasts; on a coinbase-maturity rejection mines 100 blocks and asks to retry.
    async fn send(&self, raw: &[u8]) -> Result<String, String> {
        self.client
            .send_raw_transaction(raw)
            .await
            .map_err(|e| e.to_string())
    }

    async fn mine(&mut self, n: u32) -> zol_zcash::scan::SyncReport {
        self.client.generate(n).await.unwrap();
        self.sync().await
    }

    fn next(&self) -> u32 {
        self.scanner.next_height()
    }
}

fn session_memo(app: &AppKeys, sess: &SigningKey, refund_to: [u8; 43]) -> [u8; 512] {
    let mut m = DepositMemo::new(
        0,
        sess.verifying_key().to_bytes(),
        refund_to,
        10_000,
        [0; 32],
        b"regtest",
    )
    .unwrap();
    m.set_signature(
        sess.sign(&m.signing_message(Network::Regtest, &app.vault_address()))
            .to_bytes(),
    );
    m.encode_text().unwrap().0
}

#[tokio::test]
#[ignore]
async fn regtest_deposit_genesis_refund() {
    let mut e = setup(ZebraClient::new(url(
        "ZOL_ZEBRA_URL",
        "http://127.0.0.1:18232/",
    )))
    .await;
    let coin = e.fund().await;
    // Amounts relative to the coinbase: regtest halves the subsidy every few hundred blocks.
    let deposit = coin.value / 4;

    // Deposit: wallet pays 1 ZEC to an app session receiver with a ZOL1 text memo.
    let sess = SigningKey::generate(&mut rand::rngs::OsRng);
    let (recv, idx) = e.app.session_receiver(&sess.verifying_key().to_bytes());
    let refund_to = e
        .fvk
        .address_at(42u32, Scope::External)
        .to_raw_address_bytes();
    let memo = session_memo(&e.app, &sess, refund_to);
    let mut from = e.scanner.spendable(&coin.cmx, None).unwrap();
    let mut attempt = 0;
    let dep_txid = loop {
        let (raw, _) = testing::wallet_send(
            &P(),
            &e.sk,
            &from.note,
            &from.path,
            from.anchor,
            &recv,
            deposit,
            memo,
            e.next(),
        )
        .unwrap();
        match e.send(&raw).await {
            Ok(txid) => break txid,
            Err(err) if attempt == 0 => {
                println!("first broadcast rejected: {err}; mining 100 blocks (coinbase maturity?) and retrying");
                attempt += 1;
                e.mine(100).await;
                from = e.scanner.spendable(&coin.cmx, None).unwrap();
            }
            Err(err) => panic!("deposit rejected: {err}"),
        }
    };
    println!(
        "deposit broadcast: {dep_txid} (attempt {attempt}; coinbase spend {})",
        if attempt == 0 {
            "accepted immediately"
        } else {
            "needed maturity"
        }
    );
    let rep = e.mine(1).await;
    let dep = rep
        .events
        .iter()
        .flat_map(|x| x.received.iter())
        .find(|n| n.app_id == e.app.app_id)
        .expect("deposit scanned")
        .clone();
    assert_eq!(
        (dep.value, dep.receiver, dep.receiver_index),
        (deposit, recv, idx)
    );
    assert_eq!(display_hex(&dep.txid), dep_txid);
    let dm = dep.deposit_memo().unwrap();
    dm.verify_signature(Network::Regtest, &e.app.vault_address())
        .unwrap();
    println!(
        "deposit scanned at height {}: receiver idx {}, memo verified, refund to memo address",
        dep.height,
        hex::encode(idx)
    );
    let change = rep
        .events
        .iter()
        .flat_map(|x| x.received.iter())
        .find(|n| n.app_id == WALLET)
        .unwrap()
        .clone();

    // Genesis head from the wallet's change.
    let fsp = e.scanner.spendable(&change.cmx, None).unwrap();
    let g = genesis::build(&P(), &e.sk, &fsp, &e.app, deposit / 2, e.next(), 40).unwrap();
    let gtxid = e.send(&g.raw_tx).await.expect("genesis accepted");
    let rep = e.mine(1).await;
    let head = rep
        .events
        .iter()
        .flat_map(|x| x.received.iter())
        .find(|n| n.app_id == e.app.app_id)
        .expect("head scanned");
    assert_eq!(head.cmx, g.head_cmx);
    assert_eq!(head.scope, Scope::Internal);
    assert!(HeadMemo::decode(&head.memo).unwrap().is_genesis());
    println!(
        "genesis {gtxid}: head cmx {} (activate_app value), {} zat, internal scope",
        hex::encode(g.head_cmx),
        head.value
    );

    // Refund the deposit with 2-of-3 FROST (signers 1 and 3).
    let sp = e.scanner.spendable(&dep.cmx, None).unwrap();
    let r = refund::build(&P(), &e.app, &sp, &dm.payout_address, e.next(), 20).unwrap();
    let signers: BTreeMap<_, _> = e
        .kps
        .iter()
        .filter(|(i, _)| **i != frost::identifier(2))
        .map(|(i, k)| (*i, k.clone()))
        .collect();
    let r = refund::sign_with_frost(
        r,
        &e.app,
        &sp.note,
        &signers,
        &e.pk,
        e.scanner.tip().0,
        &mut rand::rngs::OsRng,
    )
    .unwrap();
    let (raw, txid) = refund::extract(r, &P()).unwrap();
    let rtxid = e.send(&raw).await.expect("refund accepted");
    assert_eq!(rtxid, display_hex(&txid));
    let rep = e.mine(1).await;
    let spent = rep
        .events
        .iter()
        .flat_map(|x| x.spent.iter())
        .find(|s| s.cmx == dep.cmx)
        .expect("deposit spend detected");
    assert_eq!(display_hex(&spent.spending_txid), rtxid);
    let back = rep
        .events
        .iter()
        .flat_map(|x| x.received.iter())
        .find(|n| n.app_id == WALLET && n.receiver == refund_to)
        .expect("player got refund");
    assert_eq!(back.value, deposit - 10_000);
    println!(
        "refund {rtxid} mined at {}: deposit spent, player received {} at the memo address",
        spent.height, back.value
    );
    let v = e.client.get_raw_transaction_verbose(&rtxid).await.unwrap();
    println!(
        "getrawtransaction: height={} confirmations={}",
        v["height"], v["confirmations"]
    );

    // Snapshot round trip at the end.
    let snap = e.scanner.snapshot().unwrap();
    let sc2 = Scanner::restore(
        &snap,
        vec![
            Account::from(&e.app),
            Account {
                id: WALLET,
                fvk: e.fvk.clone(),
            },
        ],
    )
    .unwrap();
    assert_eq!(sc2.tip(), e.scanner.tip());
    println!("snapshot {} bytes restored", snap.len());
}

/// JSON-RPC to a node through `docker exec` (works while the node is partitioned; its
/// published host port disappears when it leaves the docker network).
fn exec_rpc(node: &str, method: &str, params: &str) -> serde_json::Value {
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let out = Command::new("docker")
        .args([
            "exec",
            &format!(
                "zol-{}-{node}",
                std::env::var("ZOL_ZEBRA").unwrap_or_else(|_| "zebra".into())
            ),
            "curl",
            "-s",
            "--max-time",
            "900",
            "-H",
            "content-type: application/json",
            "--data",
            &body,
            "http://127.0.0.1:18232/",
        ])
        .output()
        .expect("docker exec");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("rpc json");
    assert!(v["error"].is_null(), "{node} {method}: {}", v["error"]);
    v["result"].clone()
}

async fn wait_same_tip(secs: u64) -> bool {
    for _ in 0..secs {
        if exec_rpc("a", "getbestblockhash", "[]") == exec_rpc("b", "getbestblockhash", "[]") {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

#[tokio::test]
#[ignore]
async fn regtest_reorg_drops_deposit() {
    let a = ZebraClient::new(url("ZOL_ZEBRA_URL", "http://127.0.0.1:18232/"));
    exec_rpc("b", "getblockcount", "[]"); // node b up (spikes/zebra/scripts/up.sh pair)
    script("peer.sh");
    a.generate(1).await.unwrap();
    assert!(wait_same_tip(180).await, "a and b converge before the test");
    let mut e = setup(a.clone()).await;
    let coin = e.fund().await;
    e.mine(1).await; // shielded coinbase needs no maturity; one block so b syncs it
    assert!(wait_same_tip(180).await, "b synced the funding");

    script("partition.sh");
    let sess = SigningKey::generate(&mut rand::rngs::OsRng);
    let (recv, _) = e.app.session_receiver(&sess.verifying_key().to_bytes());
    let memo = session_memo(
        &e.app,
        &sess,
        e.fvk
            .address_at(1u32, Scope::External)
            .to_raw_address_bytes(),
    );
    let from = e.scanner.spendable(&coin.cmx, None).unwrap();
    let (raw, _) = testing::wallet_send(
        &P(),
        &e.sk,
        &from.note,
        &from.path,
        from.anchor,
        &recv,
        coin.value / 2,
        memo,
        e.next(),
    )
    .unwrap();
    let txid = e.send(&raw).await.expect("deposit to a");
    let rep = e.mine(1).await;
    let dep = rep
        .events
        .iter()
        .flat_map(|x| x.received.iter())
        .find(|n| n.app_id == e.app.app_id)
        .expect("deposit on a")
        .clone();
    let fork_base = dep.height - 1;
    println!("a: deposit {txid} at height {} (partitioned)", dep.height);
    exec_rpc("b", "generate", "[3]");
    println!(
        "b: mined 3 blocks on its fork, height {}",
        exec_rpc("b", "getblockcount", "[]")
    );

    script("peer.sh");
    assert!(wait_same_tip(300).await, "a reorgs onto b's longer chain");
    println!(
        "healed: a height {} tip {}",
        a.get_block_count().await.unwrap(),
        a.get_best_block_hash().await.unwrap()
    );
    let rep = e.sync().await;
    println!(
        "scanner sync: rewound_to={:?}, blocks scanned {}",
        rep.rewound_to, rep.blocks
    );
    assert_eq!(rep.rewound_to, Some(fork_base));
    assert!(
        e.scanner.note_by_cmx(&dep.cmx).is_none(),
        "deposit dropped by the rewind"
    );
    assert!(
        e.scanner.spendable(&coin.cmx, None).is_ok(),
        "funding note unspent again and still witnessed"
    );
    let in_mempool = a.get_raw_mempool().await.unwrap().contains(&txid);
    println!("deposit tx back in a's mempool after the reorg: {in_mempool}");
}

/// Regenerates `tests/fixtures/coinbase_block.json` (only with `ZOL_REGEN_FIXTURE=1`): a regtest block whose coinbase pays an
/// Ironwood note to the fixed key `SpendingKey([7; 32])`, for the offline block-parsing test.
#[tokio::test]
#[ignore]
async fn regtest_make_block_fixture() {
    if std::env::var("ZOL_REGEN_FIXTURE").is_err() {
        println!(
            "skipped: set ZOL_REGEN_FIXTURE=1 to regenerate tests/fixtures/coinbase_block.json"
        );
        return;
    }
    let client = ZebraClient::new(url("ZOL_ZEBRA_URL", "http://127.0.0.1:18232/"));
    let sk = SpendingKey::from_bytes([7; 32]).unwrap();
    let fvk = FullViewingKey::from(&sk);
    let ua = encode_ua(
        &P(),
        &fvk.address_at(0u32, Scope::External).to_raw_address_bytes(),
    );
    let h = client.generate_to_address(1, &ua).await.unwrap().remove(0);
    let raw = client.get_block_raw_by_hash(&h).await.unwrap();
    let height = client.get_block_count().await.unwrap();
    let prev = client.get_block_hash(height - 1).await.unwrap();
    let ts = client
        .z_get_treestate(&(height - 1).to_string())
        .await
        .unwrap();
    let fixture = serde_json::json!({
        "height": height, "hash": h, "prev_hash": prev,
        "ironwood_final_state_prev": ts.pointer("/ironwood/commitments/finalState"),
        "raw": hex::encode(&raw),
    });
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/coinbase_block.json"
    );
    std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap()).unwrap();
    println!("wrote {path}: height {height}, {} bytes", raw.len());
}
