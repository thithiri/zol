//! Spawns `zol-zcashd --dev` against the regtest pair (spikes/zebra) and drives the API.
//! Ignored by default:
//! `cargo test --test zcashd -- --ignored --nocapture`

use std::{process::Child, time::Duration};

use ed25519_dalek::{Signer as _, SigningKey};
use rand::RngCore;
use zol_types::{api, ChainEvent, Scope};
use zol_wire::{DepositMemo, Network};
use zol_zcash::zebra::ZebraClient;

struct Daemon {
    child: Child,
    base: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(cfg: &std::path::Path, port: u16) -> Daemon {
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_zol-zcashd"))
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--dev",
            "--dev-seed",
            "zcashd-test",
        ])
        .spawn()
        .expect("spawn zol-zcashd");
    Daemon {
        child,
        base: format!("http://127.0.0.1:{port}"),
    }
}

async fn get<T: serde::de::DeserializeOwned>(d: &Daemon, path: &str) -> T {
    let r = reqwest::get(format!("{}{path}", d.base)).await.unwrap();
    let st = r.status();
    let txt = r.text().await.unwrap();
    assert!(st.is_success(), "GET {path}: {st} {txt}");
    serde_json::from_str(&txt).unwrap()
}

async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    d: &Daemon,
    path: &str,
    body: &B,
) -> Result<T, String> {
    let r = reqwest::Client::new()
        .post(format!("{}{path}", d.base))
        .json(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    let txt = r.text().await.unwrap();
    if !st.is_success() {
        return Err(format!("{st} {txt}"));
    }
    Ok(serde_json::from_str(&txt).unwrap())
}

async fn wait_health(d: &Daemon) -> serde_json::Value {
    for _ in 0..120 {
        if let Ok(r) = reqwest::get(format!("{}/health", d.base)).await {
            if r.status().is_success() {
                return r.json().await.unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("zol-zcashd did not come up");
}

/// Waits until the daemon's log reaches Zebra's tip.
async fn wait_synced(d: &Daemon, z: &ZebraClient) {
    let h = z.get_block_count().await.unwrap();
    for _ in 0..120 {
        let e: api::Events = get(d, "/events?after=0&limit=1").await;
        if e.tip >= h {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("daemon did not sync to {h}");
}

async fn all_events(d: &Daemon) -> Vec<(u64, ChainEvent)> {
    let mut out = vec![];
    let mut after = 0;
    loop {
        let e: api::Events = get(d, &format!("/events?after={after}&limit=3")).await;
        if e.events.is_empty() {
            return out;
        }
        after = e.next;
        out.extend(e.events);
    }
}

#[tokio::test]
#[ignore]
async fn zcashd_dev_flow() {
    let z = ZebraClient::new("http://127.0.0.1:18232/");
    z.get_block_count()
        .await
        .expect("regtest up (spikes/zebra/scripts/up.sh)");
    let mut app = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut app);
    let app_hex = hex::encode(app);
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("zol-zcashd-test-{}", &app_hex[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("zcashd.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"listen = "127.0.0.1:{port}"
network = "regtest"
state_dir = "{}"
poll_ms = 200
apps = ["{app_hex}"]
[zebra]
url = "http://127.0.0.1:18232/"
"#,
            dir.join("state").display()
        ),
    )
    .unwrap();

    // Amounts scale with the current block subsidy (regtest halves it every few hundred
    // blocks): the deposit needs about 3 coinbase notes, so dev funding must combine notes.
    let sub: serde_json::Value = z
        .call("getblocksubsidy", serde_json::json!([]))
        .await
        .unwrap();
    let subsidy = (sub["miner"].as_f64().unwrap() * 1e8).round() as u64;
    let deposit = (subsidy * 5 / 2).max(100_000);
    let float = (subsidy / 2).max(50_000);
    println!("block subsidy {subsidy} zat: genesis float {float}, deposit {deposit}");

    let d = spawn(&cfg, port);
    let h = wait_health(&d).await;
    println!("health: {h}");
    assert_eq!(h["mode"], "dev");

    let v: api::Vault = get(&d, &format!("/vault?app={app_hex}")).await;
    assert!(v.ua.starts_with("uregtest1"));
    let sess = SigningKey::generate(&mut rand::rngs::OsRng);
    let sr: api::SessionReceiver = get(
        &d,
        &format!(
            "/session_receiver?app={app_hex}&session={}",
            hex::encode(sess.verifying_key().to_bytes())
        ),
    )
    .await;
    let valid: serde_json::Value = get(
        &d,
        &format!("/valid_receiver?raw={}", hex::encode(&sr.receiver)),
    )
    .await;
    assert_eq!(valid["valid"], true);
    let invalid: serde_json::Value = get(
        &d,
        &format!("/valid_receiver?raw={}", hex::encode([0u8; 43])),
    )
    .await;
    println!("valid_receiver(zeros) = {invalid}");

    // Genesis head from the dev wallet (mines a coinbase to itself first).
    let g: api::GenesisResponse = post(&d, "/dev/genesis", &api::GenesisRequest { app, float })
        .await
        .unwrap();
    println!(
        "genesis txid {} head {}",
        zol_zcash::display_hex(&g.txid),
        hex::encode(g.head_cmx)
    );
    z.generate(1).await.unwrap();
    wait_synced(&d, &z).await;

    // A "player" deposit to the session receiver with a ZOL1 text memo.
    // The player's own (non-vault) receiver, which the memo names for refunds.
    let refund_to: [u8; 43] = {
        let sk = zol_zcash::testing::random_spending_key(&mut rand::rngs::OsRng);
        orchard::keys::FullViewingKey::from(&sk)
            .address_at(0u32, orchard::keys::Scope::External)
            .to_raw_address_bytes()
    };
    let addr: [u8; 43] = v.address.clone().try_into().unwrap();
    let mut m = DepositMemo::new(
        0,
        sess.verifying_key().to_bytes(),
        refund_to,
        1_000_000,
        [0; 32],
        b"zcashd",
    )
    .unwrap();
    m.set_signature(
        sess.sign(&m.signing_message(Network::Regtest, &addr))
            .to_bytes(),
    );
    let (memo, len) = m.encode_text().unwrap();
    let s: api::SendResponse = post(
        &d,
        "/dev/send",
        &api::SendRequest {
            to: sr.receiver.clone(),
            value: deposit,
            memo: memo[..len].to_vec(),
        },
    )
    .await
    .unwrap();
    println!("deposit txid {}", zol_zcash::display_hex(&s.txid));
    let raw = z
        .get_raw_transaction(&zol_zcash::display_hex(&s.txid))
        .await
        .unwrap();
    let inputs = zol_zcash::verify::nullifiers(&raw).unwrap().len();
    println!("deposit spends {inputs} dev wallet notes");
    if subsidy * 5 / 2 >= 100_000 {
        assert!(inputs >= 2, "dev funding combined several notes");
    }
    z.generate(1).await.unwrap();
    wait_synced(&d, &z).await;

    let evs = all_events(&d).await;
    let dep = evs
        .iter()
        .find_map(|(_, e)| match e {
            ChainEvent::Note(n) if n.receiver == sr.receiver => Some(n.clone()),
            _ => None,
        })
        .expect("deposit Note event");
    assert_eq!(
        (dep.value, dep.receiver_index, dep.scope, dep.txid),
        (deposit, sr.index, Scope::External, s.txid)
    );
    assert_eq!(&dep.memo[..len], &memo[..len]);
    let head = evs
        .iter()
        .find_map(|(_, e)| match e {
            ChainEvent::Note(n) if n.cmx == g.head_cmx => Some(n.clone()),
            _ => None,
        })
        .expect("head Note event");
    assert_eq!(head.scope, Scope::Internal);
    assert!(evs
        .iter()
        .any(|(_, e)| matches!(e, ChainEvent::Block { height, .. } if *height == dep.height)));
    // Seqs are contiguous from 1 and Note precedes its Block.
    for (i, (s, _)) in evs.iter().enumerate() {
        assert_eq!(*s, i as u64 + 1);
    }
    let note_pos = evs
        .iter()
        .position(|(_, e)| matches!(e, ChainEvent::Note(n) if n.cmx == dep.cmx))
        .unwrap();
    let block_pos = evs
        .iter()
        .position(|(_, e)| matches!(e, ChainEvent::Block { height, .. } if *height == dep.height))
        .unwrap();
    assert!(note_pos < block_pos);
    println!(
        "events: {} so far; deposit Note at seq {}, its Block at seq {}",
        evs.len(),
        evs[note_pos].0,
        evs[block_pos].0
    );

    // Refunds: reject a head; refund the deposit; idempotent while pending.
    let e = post::<_, api::RefundResponse>(
        &d,
        "/refund",
        &api::RefundRequest {
            app,
            cmx: g.head_cmx,
            to: refund_to.to_vec(),
        },
    )
    .await
    .unwrap_err();
    println!("refund of the head: {e}");
    let r1: api::RefundResponse = post(
        &d,
        "/refund",
        &api::RefundRequest {
            app,
            cmx: dep.cmx,
            to: refund_to.to_vec(),
        },
    )
    .await
    .unwrap();
    assert!(!r1.existing);
    assert_eq!((r1.value, r1.fee), (deposit - 10_000, 10_000));
    let r2: api::RefundResponse = post(
        &d,
        "/refund",
        &api::RefundRequest {
            app,
            cmx: dep.cmx,
            to: refund_to.to_vec(),
        },
    )
    .await
    .unwrap();
    assert!(r2.existing && r2.txid == r1.txid);
    let e = post::<_, api::RefundResponse>(
        &d,
        "/refund",
        &api::RefundRequest {
            app,
            cmx: dep.cmx,
            to: sr.receiver.clone(),
        },
    )
    .await
    .unwrap_err();
    println!("refund to a different receiver while pending: {e}");
    println!(
        "refund txid {} (second call existing=true)",
        zol_zcash::display_hex(&r1.txid)
    );
    z.generate(1).await.unwrap();
    wait_synced(&d, &z).await;
    let evs = all_events(&d).await;
    let spent = evs.iter().find(|(_, e)| matches!(e, ChainEvent::Spent { nullifier, .. } if *nullifier == dep.nullifier)).expect("Spent event");
    match &spent.1 {
        ChainEvent::Spent { txid, app: a, .. } => assert!(*txid == r1.txid && *a == app),
        _ => unreachable!(),
    }
    println!("Spent event at seq {}", spent.0);

    // Restart: the log and cursors survive; the mined refund is still idempotent.
    drop(d);
    let d = spawn(&cfg, port);
    wait_health(&d).await;
    let evs2 = all_events(&d).await;
    assert_eq!(
        evs2[..evs.len()],
        evs[..],
        "same seqs and events after restart"
    );
    let r3: api::RefundResponse = post(
        &d,
        "/refund",
        &api::RefundRequest {
            app,
            cmx: dep.cmx,
            to: refund_to.to_vec(),
        },
    )
    .await
    .unwrap();
    assert!(r3.existing && r3.txid == r1.txid);
    let last = evs2.last().unwrap().0;
    z.generate(1).await.unwrap();
    wait_synced(&d, &z).await;
    let more: api::Events = get(&d, &format!("/events?after={last}")).await;
    assert!(more.events.iter().all(|(s, _)| *s > last) && !more.events.is_empty());
    println!(
        "after restart: {} events replayed identically; new events continue from seq {}",
        evs2.len(),
        more.events[0].0
    );
    let _ = std::fs::remove_dir_all(&dir);
}
