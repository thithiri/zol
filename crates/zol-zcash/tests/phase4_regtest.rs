//! Phase 4 on regtest (infra/zakura pair): head transitions signed by three `zol-zcashd`
//! processes holding ONE Pallas FROST share each (2-of-3), over /payout/* and /sign/*.
//! Ignored by default:
//! `cargo test --test phase4_regtest -- --ignored --nocapture --test-threads=1`
//! Env: `ZOL_NODE_URL` (default http://127.0.0.1:18232/), `ZOL_DEV_SEED` (default "zol-dev",
//! the seed whose dev wallet UA is the node's miner_address). The reorg test drives the
//! pair with `docker exec` (containers zol-zakura-a/b) and `infra/zakura/scripts`.

use std::{process::Child, time::Duration};

use rand::RngCore;
use zol_types::{api, ChainEvent};
use zol_zcash::{display_hex, node::NodeClient};

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

fn seed() -> String {
    std::env::var("ZOL_DEV_SEED").unwrap_or_else(|_| "zol-dev".into())
}
fn node_url() -> String {
    std::env::var("ZOL_NODE_URL").unwrap_or_else(|_| "http://127.0.0.1:18232/".into())
}
const BIN: &str = env!("CARGO_BIN_EXE_zol-zcashd");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Daemons 0 and 1: `--dev --dev-share i` (single-share, plus the dev wallet). Daemon 2: a
/// plain operator with one share from the config (no --dev).
fn spawn(dir: &std::path::Path, app_hex: &str, i: usize) -> Daemon {
    let port = free_port();
    let state = dir.join(format!("state{i}"));
    let mut cfg = format!(
        "listen = \"127.0.0.1:{port}\"\nnetwork = \"regtest\"\nstate_dir = \"{}\"\npoll_ms = 200\napps = [\"{app_hex}\"]\n[node]\nurl = \"{}\"\n",
        state.display(),
        node_url()
    );
    let mut args = vec![
        "--config".to_string(),
        dir.join(format!("d{i}.toml")).display().to_string(),
    ];
    if i < 2 {
        args.extend([
            "--dev".into(),
            "--dev-seed".into(),
            seed(),
            "--dev-share".into(),
            i.to_string(),
        ]);
    } else {
        // Operator mode: take the [keys] section, keep only share 2.
        let out = std::process::Command::new(BIN)
            .args(["--print-dev-keys", "--dev-seed", &seed()])
            .output()
            .unwrap();
        let keys = String::from_utf8(out.stdout).unwrap();
        let mut lines: Vec<String> = vec![];
        let mut in_shares = false;
        let mut shares = vec![];
        for l in keys.lines() {
            if l.starts_with("shares = [") {
                in_shares = true;
                continue;
            }
            if in_shares {
                if l.trim() == "]" {
                    in_shares = false;
                    lines.push(format!("shares = [{}]", shares[2]));
                } else {
                    shares.push(l.trim().trim_end_matches(',').to_string());
                }
                continue;
            }
            lines.push(l.to_string());
        }
        cfg.push_str(&lines.join("\n"));
    }
    std::fs::write(dir.join(format!("d{i}.toml")), cfg).unwrap();
    let child = std::process::Command::new(BIN).args(&args).spawn().unwrap();
    Daemon {
        child,
        base: format!("http://127.0.0.1:{port}"),
    }
}

async fn get<T: serde::de::DeserializeOwned>(d: &Daemon, path: &str) -> T {
    let r = reqwest::get(format!("{}{path}", d.base)).await.unwrap();
    let st = r.status();
    let t = r.text().await.unwrap();
    assert!(st.is_success(), "GET {path}: {st} {t}");
    serde_json::from_str(&t).unwrap()
}

async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    d: &Daemon,
    path: &str,
    b: &B,
) -> Result<T, String> {
    let r = reqwest::Client::new()
        .post(format!("{}{path}", d.base))
        .json(b)
        .send()
        .await
        .unwrap();
    let st = r.status();
    let t = r.text().await.unwrap();
    if !st.is_success() {
        return Err(format!("{st} {t}"));
    }
    Ok(serde_json::from_str(&t).unwrap())
}

async fn health(d: &Daemon) -> serde_json::Value {
    for _ in 0..240 {
        if let Ok(r) = reqwest::get(format!("{}/health", d.base)).await {
            if r.status().is_success() {
                return r.json().await.unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("daemon did not start");
}

async fn synced(ds: &[&Daemon], z: &NodeClient) {
    let h = z.get_block_count().await.unwrap();
    for d in ds {
        for i in 0..240 {
            let e: api::Events = get(d, "/events?after=0&limit=1").await;
            if e.tip >= h {
                break;
            }
            assert!(i < 239, "daemon did not sync to {h}");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

async fn mine(z: &NodeClient, ds: &[&Daemon], n: u32) {
    z.generate(n).await.unwrap();
    synced(ds, z).await;
}

async fn head(d: &Daemon, app: &str, genesis: &[u8; 32]) -> api::HeadChain {
    get(
        d,
        &format!("/head?app={app}&genesis={}", hex::encode(genesis)),
    )
    .await
}

/// Distributed 2-of-3 signing of `pczt`: commit + share on `signers`, aggregate on `agg`.
async fn sign(
    signers: &[&Daemon],
    agg: &Daemon,
    session: u64,
    kind: api::SignKind,
    pczt: &[u8],
) -> Result<api::Broadcast, String> {
    let mut comms = vec![];
    for d in signers {
        comms.push(
            post::<_, api::CommitResponse>(
                d,
                "/sign/commit",
                &api::CommitRequest {
                    session,
                    kind: kind.clone(),
                    pczt: pczt.to_vec(),
                },
            )
            .await?,
        );
    }
    let mut shares = vec![];
    for d in signers {
        shares.push(
            post::<_, api::ShareResponse>(
                d,
                "/sign/share",
                &api::ShareRequest {
                    session,
                    commitments: comms.clone(),
                },
            )
            .await?,
        );
    }
    post(
        agg,
        "/sign/aggregate",
        &api::AggregateRequest {
            session,
            pczt: pczt.to_vec(),
            commitments: comms,
            shares,
        },
    )
    .await
}

fn receiver() -> Vec<u8> {
    let sk = zol_zcash::testing::random_spending_key(&mut rand::rngs::OsRng);
    orchard::keys::FullViewingKey::from(&sk)
        .address_at(0u32, orchard::keys::Scope::External)
        .to_raw_address_bytes()
        .to_vec()
}

struct Env {
    z: NodeClient,
    dir: std::path::PathBuf,
    app: [u8; 32],
    app_hex: String,
    d: Vec<Daemon>,
    genesis: [u8; 32],
    subsidy: u64,
}

impl Env {
    fn refs(&self) -> Vec<&Daemon> {
        self.d.iter().collect()
    }
    async fn deposit(&self, value: u64) -> [u8; 32] {
        let mut sess = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut sess);
        let sr: api::SessionReceiver = get(
            &self.d[0],
            &format!(
                "/session_receiver?app={}&session={}",
                self.app_hex,
                hex::encode(sess)
            ),
        )
        .await;
        let s: api::SendResponse = post(
            &self.d[0],
            "/dev/send",
            &api::SendRequest {
                to: sr.receiver.clone(),
                value,
                memo: vec![],
            },
        )
        .await
        .unwrap();
        mine(&self.z, &self.refs(), 1).await;
        let evs: api::Events = get(&self.d[0], "/events?after=0&limit=10000").await;
        evs.events
            .iter()
            .find_map(|(_, e)| match e {
                ChainEvent::Note(n) if n.txid == s.txid && n.receiver == sr.receiver => Some(n.cmx),
                _ => None,
            })
            .unwrap()
    }
    fn plan(
        &self,
        seq: Option<u64>,
        amount: u64,
        to: &[u8],
        credited: Vec<[u8; 32]>,
    ) -> api::PayoutPlan {
        api::PayoutPlan {
            app: self.app,
            genesis_head: self.genesis,
            records: seq
                .map(|s| {
                    vec![api::PayoutRecord {
                        sequence: s,
                        payouts: vec![api::PayoutLine {
                            commitment: [s as u8 + 1; 32],
                            amount,
                            to: to.to_vec(),
                        }],
                    }]
                })
                .unwrap_or_default(),
            credited,
            fee_pool_accrued: 200_000,
            claims_after: 0,
            action_target: 0,
            max_inputs: 4,
            full_depth: 1,
            head_depth: 1,
            expiry_delta: 40,
        }
    }
}

async fn setup(tag: &str) -> Env {
    let z = NodeClient::new(node_url());
    z.get_block_count()
        .await
        .expect("regtest up (infra/zakura/scripts/up.sh pair)");
    let sub: serde_json::Value = z
        .call("getblocksubsidy", serde_json::json!([]))
        .await
        .unwrap();
    let subsidy = (sub["miner"].as_f64().unwrap() * 1e8).round() as u64;
    assert!(
        subsidy >= 10_000_000,
        "regtest subsidy {subsidy} too low: restart the chain"
    );
    let mut app = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut app);
    let app_hex = hex::encode(app);
    let dir = std::env::temp_dir().join(format!("zol-p4-{tag}-{}", &app_hex[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let d: Vec<Daemon> = (0..3).map(|i| spawn(&dir, &app_hex, i)).collect();
    for x in &d {
        let h = health(x).await;
        println!(
            "daemon {}: mode {} signers_held {}",
            x.base, h["mode"], h["signers_held"]
        );
        assert_eq!(h["signers_held"], 1);
    }
    let g: api::GenesisResponse = post(
        &d[0],
        "/dev/genesis",
        &api::GenesisRequest {
            app,
            float: subsidy / 4,
        },
    )
    .await
    .unwrap();
    let mut env = Env {
        z,
        dir,
        app,
        app_hex,
        d,
        genesis: g.head_cmx,
        subsidy,
    };
    mine(&env.z, &env.refs(), 1).await;
    env.genesis = g.head_cmx;
    env
}

#[tokio::test]
#[ignore]
async fn phase4_distributed_payouts_consolidation_and_double_spend() {
    let e = setup("e2e").await;
    let (app, s) = (&e.app_hex, e.subsidy);
    let h = head(&e.d[2], app, &e.genesis).await;
    assert!(h.fault.is_none() && h.chain.len() == 1, "{h:?}");
    println!(
        "genesis head {} value {}",
        hex::encode(e.genesis),
        h.chain[0].value
    );

    // Payout 1: record 0 needs the credited deposit (amount > float).
    let d1 = e.deposit(s / 2).await;
    let p1 = receiver();
    let plan = e.plan(Some(0), s / 4 + s / 8, &p1, vec![d1]);
    let prop: api::Proposal = post(&e.d[0], "/payout/propose", &plan).await.unwrap();
    for d in &e.d {
        let v: api::VerifyResponse = post(
            d,
            "/payout/verify",
            &api::VerifyRequest {
                plan: plan.clone(),
                pczt: prop.pczt.clone(),
            },
        )
        .await
        .unwrap();
        assert!(v.ok, "{:?}", v.failures);
        assert_eq!(v.sighash, prop.sighash);
    }
    // A plan that disagrees (another amount) is refused at commit.
    let mut wrong = plan.clone();
    wrong.records[0].payouts[0].amount -= 1;
    let err = post::<_, api::CommitResponse>(
        &e.d[1],
        "/sign/commit",
        &api::CommitRequest {
            session: 99,
            kind: api::SignKind::Payout { plan: wrong },
            pczt: prop.pczt.clone(),
        },
    )
    .await
    .unwrap_err();
    println!("commit with a disagreeing plan: {err}");
    let b = sign(
        &[&e.d[0], &e.d[1]],
        &e.d[2],
        1,
        api::SignKind::Payout { plan: plan.clone() },
        &prop.pczt,
    )
    .await
    .unwrap();
    assert_eq!(b.txid, prop.sighash, "txid = sighash");
    println!(
        "payout 1 broadcast {} (inputs {}, fee {}, new head {})",
        display_hex(&b.txid),
        prop.inputs.len(),
        prop.fee,
        prop.new_head_value
    );
    let hp = head(&e.d[2], app, &e.genesis).await;
    assert_eq!(
        hp.pending,
        Some(b.txid),
        "aggregator knows the pending transition"
    );
    // A package without this signer's own commitments is refused.
    let foreign = post::<_, api::ShareResponse>(
        &e.d[0],
        "/sign/share",
        &api::ShareRequest {
            session: 1,
            commitments: vec![],
        },
    )
    .await
    .unwrap_err();
    println!("/sign/share without our commitments: {foreign}");
    mine(&e.z, &e.refs(), 1).await;
    for d in &e.d {
        let h = head(d, app, &e.genesis).await;
        assert!(h.fault.is_none() && h.pending.is_none(), "{h:?}");
        assert_eq!(h.chain.len(), 2);
        assert_eq!(
            (h.chain[1].position, h.chain[1].paid_count, h.chain[1].txid),
            (1, 1, b.txid)
        );
    }

    // Payout 2 from the head alone; signers 1+2 (operator), aggregator 0.
    let plan2 = e.plan(Some(1), s / 16, &receiver(), vec![]);
    let prop2: api::Proposal = post(&e.d[1], "/payout/propose", &plan2).await.unwrap();
    let b2 = sign(
        &[&e.d[1], &e.d[2]],
        &e.d[0],
        2,
        api::SignKind::Payout {
            plan: plan2.clone(),
        },
        &prop2.pczt,
    )
    .await
    .unwrap();
    mine(&e.z, &e.refs(), 1).await;
    let h = head(&e.d[0], app, &e.genesis).await;
    assert_eq!(
        (h.chain.len(), h.chain[2].paid_count, h.chain[2].txid),
        (3, 2, b2.txid)
    );
    println!("payout 2 {} mined; head position 2", display_hex(&b2.txid));

    // Consolidation: no records; sweeps a new credited deposit; padded to 4 actions.
    let d2 = e.deposit(s / 4).await;
    let mut cons = e.plan(None, 0, &[], vec![d2]);
    cons.action_target = 4;
    let cp: api::Proposal = post(&e.d[2], "/payout/propose", &cons).await.unwrap();
    assert!(cp.inputs.contains(&d2) && cp.fee == 20_000);
    let bc = sign(
        &[&e.d[0], &e.d[2]],
        &e.d[2],
        3,
        api::SignKind::Payout { plan: cons.clone() },
        &cp.pczt,
    )
    .await
    .unwrap();
    mine(&e.z, &e.refs(), 1).await;
    let h = head(&e.d[1], app, &e.genesis).await;
    assert_eq!(
        (h.chain.len(), h.chain[3].paid_count, h.chain[3].txid),
        (4, 2, bc.txid)
    );
    println!(
        "consolidation {} mined: head {} (swept deposit)",
        display_hex(&bc.txid),
        h.chain[3].value
    );

    // Two different valid proposals for record 2, both fully signed: they share the head's
    // nullifier, so only one can confirm.
    let plan4 = e.plan(Some(2), s / 32, &receiver(), vec![]);
    let pa: api::Proposal = post(&e.d[0], "/payout/propose", &plan4).await.unwrap();
    let pb: api::Proposal = post(&e.d[1], "/payout/propose", &plan4).await.unwrap();
    assert_ne!(pa.sighash, pb.sighash);
    // Commit and share both before either is broadcast (signers can't know yet).
    let ka = api::SignKind::Payout {
        plan: plan4.clone(),
    };
    let mut ca = vec![];
    let mut cb = vec![];
    for d in [&e.d[0], &e.d[1]] {
        ca.push(
            post::<_, api::CommitResponse>(
                d,
                "/sign/commit",
                &api::CommitRequest {
                    session: 10,
                    kind: ka.clone(),
                    pczt: pa.pczt.clone(),
                },
            )
            .await
            .unwrap(),
        );
    }
    for d in [&e.d[1], &e.d[2]] {
        cb.push(
            post::<_, api::CommitResponse>(
                d,
                "/sign/commit",
                &api::CommitRequest {
                    session: 11,
                    kind: ka.clone(),
                    pczt: pb.pczt.clone(),
                },
            )
            .await
            .unwrap(),
        );
    }
    let mut sa = vec![];
    let mut sb = vec![];
    for d in [&e.d[0], &e.d[1]] {
        sa.push(
            post::<_, api::ShareResponse>(
                d,
                "/sign/share",
                &api::ShareRequest {
                    session: 10,
                    commitments: ca.clone(),
                },
            )
            .await
            .unwrap(),
        );
    }
    for d in [&e.d[1], &e.d[2]] {
        sb.push(
            post::<_, api::ShareResponse>(
                d,
                "/sign/share",
                &api::ShareRequest {
                    session: 11,
                    commitments: cb.clone(),
                },
            )
            .await
            .unwrap(),
        );
    }
    // Nonces are single use: the same package again is refused.
    let reuse = post::<_, api::ShareResponse>(
        &e.d[0],
        "/sign/share",
        &api::ShareRequest {
            session: 10,
            commitments: ca.clone(),
        },
    )
    .await
    .unwrap_err();
    println!("second /sign/share with the same package: {reuse}");
    assert!(reuse.starts_with("409"));
    let ra = post::<_, api::Broadcast>(
        &e.d[0],
        "/sign/aggregate",
        &api::AggregateRequest {
            session: 10,
            pczt: pa.pczt.clone(),
            commitments: ca,
            shares: sa,
        },
    )
    .await;
    let rb = post::<_, api::Broadcast>(
        &e.d[2],
        "/sign/aggregate",
        &api::AggregateRequest {
            session: 11,
            pczt: pb.pczt.clone(),
            commitments: cb,
            shares: sb,
        },
    )
    .await;
    println!(
        "double: A broadcast {:?}; B broadcast {:?}",
        ra.as_ref().map(|b| display_hex(&b.txid)),
        rb.as_ref()
            .map(|b| display_hex(&b.txid))
            .map_err(|e| e.chars().take(160).collect::<String>())
    );
    mine(&e.z, &e.refs(), 1).await;
    let h = head(&e.d[1], app, &e.genesis).await;
    assert!(h.fault.is_none());
    assert_eq!(h.chain.len(), 5);
    assert_eq!(h.chain[4].paid_count, 3, "record 2 paid exactly once");
    let mined = h.chain[4].txid;
    assert!(mined == pa.sighash || mined == pb.sighash);
    let loser = if mined == pa.sighash {
        pb.sighash
    } else {
        pa.sighash
    };
    let lost = e.z.get_raw_transaction_verbose(&display_hex(&loser)).await;
    println!(
        "mined {}; other {} -> {:?}",
        display_hex(&mined),
        display_hex(&loser),
        lost.as_ref()
            .map(|v| v["confirmations"].clone())
            .map_err(|e| e.to_string().chars().take(100).collect::<String>())
    );
    assert!(lost.is_err() || lost.unwrap()["confirmations"].as_u64().unwrap_or(0) == 0);

    // Refund via /refund/propose + distributed signing (kind Refund).
    let d3 = e.deposit(s / 8).await;
    let to = receiver();
    let rp: api::RefundProposal = post(
        &e.d[2],
        "/refund/propose",
        &api::RefundRequest {
            app: e.app,
            cmx: d3,
            to: to.clone(),
        },
    )
    .await
    .unwrap();
    let br = sign(
        &[&e.d[0], &e.d[2]],
        &e.d[1],
        20,
        api::SignKind::Refund {
            app: e.app,
            cmx: d3,
            to: to.clone(),
        },
        &rp.pczt,
    )
    .await
    .unwrap();
    mine(&e.z, &e.refs(), 1).await;
    let evs: api::Events = get(&e.d[0], "/events?after=0&limit=10000").await;
    assert!(evs
        .events
        .iter()
        .any(|(_, ev)| matches!(ev, ChainEvent::Spent { txid, .. } if *txid == br.txid)));
    println!(
        "refund {} via /refund/propose + /sign/* (value {}) mined; Spent event seen",
        display_hex(&br.txid),
        rp.value
    );
    let h = head(&e.d[0], app, &e.genesis).await;
    assert_eq!(h.chain.len(), 5, "a refund does not touch the head");
    let _ = std::fs::remove_dir_all(&e.dir);
}

/// JSON-RPC via `docker exec` (works while a node is partitioned).
fn exec_rpc(node: &str, method: &str, params: &str) -> serde_json::Value {
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let out = std::process::Command::new("docker")
        .args([
            "exec",
            &format!("zol-zakura-{node}"),
            "curl",
            "-s",
            "-H",
            "content-type: application/json",
            "--data",
            &body,
            "http://127.0.0.1:18232/",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["error"].is_null(), "{node} {method}: {}", v["error"]);
    v["result"].clone()
}

fn script(name: &str) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../infra/zakura/scripts/");
    let _ = std::process::Command::new("bash")
        .arg(format!("{path}{name}"))
        .output();
}

async fn same_tip(secs: u64) -> bool {
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
async fn phase4_reorged_payout_restores_head_and_pays_once() {
    script("peer.sh");
    let mut e = setup("reorg").await;
    let (app, s) = (e.app_hex.clone(), e.subsidy);
    let d1 = e.deposit(s / 2).await;
    assert!(same_tip(180).await, "b synced");
    let to = receiver();
    let mut plan = e.plan(Some(0), s / 8, &to, vec![d1]);
    plan.expiry_delta = 3; // a reorged-out pending transition stops blocking the head soon

    script("partition.sh");
    let prop: api::Proposal = post(&e.d[0], "/payout/propose", &plan).await.unwrap();
    let b = sign(
        &[&e.d[0], &e.d[1]],
        &e.d[0],
        1,
        api::SignKind::Payout { plan: plan.clone() },
        &prop.pczt,
    )
    .await
    .unwrap();
    mine(&e.z, &e.refs(), 1).await;
    let h = head(&e.d[0], &app, &e.genesis).await;
    assert_eq!((h.chain.len(), h.chain[1].txid), (2, b.txid));
    println!(
        "a: payout {} mined at {}",
        display_hex(&b.txid),
        h.chain[1].height
    );
    exec_rpc("b", "generate", "[4]");
    script("peer.sh");
    assert!(same_tip(300).await, "a reorgs onto b");
    synced(&e.refs(), &e.z).await;
    let h = head(&e.d[0], &app, &e.genesis).await;
    println!(
        "after reorg: chain len {}, pending {:?}",
        h.chain.len(),
        h.pending.map(|t| display_hex(&t))
    );
    assert_eq!(h.chain.len(), 1, "the walk went back to the genesis head");
    // Old proposal is not resurrected; once its expiry passes the head is free again.
    mine(&e.z, &e.refs(), 4).await;
    let h = head(&e.d[0], &app, &e.genesis).await;
    assert!(h.pending.is_none() && h.chain.len() == 1, "{h:?}");
    let prop2: api::Proposal = post(&e.d[1], "/payout/propose", &plan).await.unwrap();
    let b2 = sign(
        &[&e.d[1], &e.d[2]],
        &e.d[1],
        2,
        api::SignKind::Payout { plan: plan.clone() },
        &prop2.pczt,
    )
    .await
    .unwrap();
    mine(&e.z, &e.refs(), 1).await;
    let h = head(&e.d[2], &app, &e.genesis).await;
    assert_eq!(
        (h.chain.len(), h.chain[1].paid_count, h.chain[1].txid),
        (2, 1, b2.txid)
    );
    // Paying record 0 a third time is refused by every verifier.
    let prop3 = post::<_, api::Proposal>(&e.d[0], "/payout/propose", &plan).await;
    println!(
        "re-pay after reorg: {} mined; a third proposal for record 0 -> {:?}",
        display_hex(&b2.txid),
        prop3.as_ref().err()
    );
    assert!(prop3.is_err());
    let v: api::VerifyResponse = post(
        &e.d[2],
        "/payout/verify",
        &api::VerifyRequest {
            plan: plan.clone(),
            pczt: prop2.pczt.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        v.failures.contains(&"records_not_next".to_string()),
        "{:?}",
        v.failures
    );
    e.d.clear();
    let _ = std::fs::remove_dir_all(&e.dir);
}
