//! `zol-zcashd`: the Zcash half of a ZOL gateway operator.
//!
//! Scans the Zcash node (Zakura) for every configured app vault, serves an append-only, sequence-numbered
//! event log, builds/signs/broadcasts deposit refunds with Pallas FROST, and (with `--dev`)
//! simulates a player wallet and the genesis funding. API: `zol_types::api`.
//! See docs/phase3-zol-zcash.md, "zol-zcashd".
//!
//! Usage: `zol-zcashd --config <file.toml> [--dev] [--dev-seed <string>]`

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use orchard::keys::{FullViewingKey, Scope as OScope, SpendingKey};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use zol_types::{api, ChainEvent};
use zol_wire::{HeadMemo, TopUpMemo};
use zol_zcash::{
    encode_ua,
    frost::{self, Identifier, KeyPackage, PublicKeyPackage},
    genesis,
    node::NodeClient,
    payout, refund,
    scan::{Account, BlockItem, Pool, ScannerConfig, SyncStep},
    testing, AppKeys, ChainParams, Network, Scanner,
};

const DEV_WALLET_ID: [u8; 32] = [0xDE; 32];
const STATE_VERSION: u32 = 2;

// ---------------------------------------------------------------- config

#[derive(Debug, Deserialize)]
struct Config {
    /// e.g. "127.0.0.1:18300"
    listen: String,
    /// "regtest" | "testnet" | "mainnet"
    network: String,
    state_dir: PathBuf,
    /// First block to scan on a fresh state dir (birthday). Default: node tip + 1.
    start_height: Option<u32>,
    #[serde(default = "default_poll")]
    poll_ms: u64,
    #[serde(default = "default_expiry")]
    expiry_delta: u32,
    /// App program IDs, hex (32 bytes).
    apps: Vec<String>,
    node: NodeCfg,
    /// Required unless `--dev`.
    keys: Option<KeysCfg>,
}

fn default_poll() -> u64 {
    500
}
fn default_expiry() -> u32 {
    zol_zcash::DEFAULT_EXPIRY_DELTA
}

#[derive(Debug, Deserialize)]
struct NodeCfg {
    url: String,
    user: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KeysCfg {
    /// Master viewing secret K, hex (32 bytes). Secret.
    master_view: String,
    /// FROST `PublicKeyPackage` (frost-core serialization), hex. `ak_group` is its key.
    public_key_package: String,
    /// FROST `KeyPackage`s (frost-core serialization), hex. One in operator mode; at least
    /// the threshold in cluster/dev mode (all shares in one process).
    shares: Vec<String>,
}

fn hex32(s: &str) -> Result<[u8; 32], String> {
    hex::decode(s.trim())
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "expected 32 bytes of hex".to_string())
}

// ---------------------------------------------------------------- state

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RefundRecord {
    cmx: [u8; 32],
    app: [u8; 32],
    to: Vec<u8>,
    txid: [u8; 32],
    value: u64,
    fee: u64,
    expiry_height: u32,
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    version: u32,
    last_seq: u64,
    log_tip: u32,
    scanner: Vec<u8>,
    refunds: Vec<RefundRecord>,
    pending: Vec<([u8; 32], PendingTx)>,
}

/// A broadcast, unmined head transition this daemon aggregated.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct PendingTx {
    txid: [u8; 32],
    expiry_height: u32,
    head_nullifier: [u8; 32],
}

/// One distributed signing session (FROST round 1 state). Nonces are single use.
struct Session {
    app: [u8; 32],
    sighash: [u8; 32],
    identifier: Identifier,
    app_kp: KeyPackage,
    /// (action index, alpha) of every vault spend.
    actions: Vec<(u32, [u8; 32])>,
    nonces: Option<BTreeMap<u32, frost::SigningNonces>>,
    commitments: Vec<(u32, Vec<u8>)>,
}

struct Inner {
    scanner: Scanner,
    refunds: BTreeMap<[u8; 32], RefundRecord>,
    /// Dev wallet notes used by unmined dev transactions: nullifier -> expiry height.
    dev_pending: BTreeMap<[u8; 32], u32>,
    last_error: Option<String>,
    /// app -> broadcast head transition.
    pending: BTreeMap<[u8; 32], PendingTx>,
    sessions: BTreeMap<u64, Session>,
}

struct Log {
    events: Vec<(u64, ChainEvent)>,
    last_seq: u64,
    tip: u32,
}

struct Dev {
    sk: SpendingKey,
    fvk: FullViewingKey,
}

struct Node {
    params: ChainParams,
    expiry_delta: u32,
    state_dir: PathBuf,
    client: NodeClient,
    apps: BTreeMap<[u8; 32], AppKeys>,
    shares: BTreeMap<Identifier, KeyPackage>,
    /// The share this daemon uses for distributed signing (/sign/*): its only share in
    /// operator mode, the first in cluster mode, share `--dev-share` in dev mode.
    signer: Option<(Identifier, KeyPackage)>,
    pubkeys: PublicKeyPackage,
    min_signers: u16,
    ak_group: [u8; 32],
    mode: &'static str,
    dev: Option<Dev>,
    inner: Mutex<Inner>,
    log: RwLock<Log>,
    /// Serializes refund and dev operations.
    ops: Mutex<()>,
}

impl Node {
    fn state_path(&self) -> PathBuf {
        self.state_dir.join("state.bin")
    }
    fn log_path(&self) -> PathBuf {
        self.state_dir.join("events.jsonl")
    }

    /// Writes state.bin atomically (tmp + rename).
    fn write_state(&self, inner: &Inner, last_seq: u64, log_tip: u32) -> Result<(), String> {
        let p = Persisted {
            version: STATE_VERSION,
            last_seq,
            log_tip,
            scanner: inner.scanner.snapshot().map_err(|e| e.to_string())?,
            refunds: inner.refunds.values().cloned().collect(),
            pending: inner.pending.iter().map(|(a, p)| (*a, *p)).collect(),
        };
        let bytes = bincode::serialize(&p).map_err(|e| e.to_string())?;
        let tmp = self.state_dir.join("state.bin.tmp");
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(&bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.state_path()).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// One sync round. Durability order: append events to the log file (fsync), then write
    /// the state (scanner + last_seq) atomically, then publish in memory. A crash between
    /// the first two leaves log lines past `last_seq`, which are truncated on restart and
    /// were never served, so cursors stay valid across restarts.
    async fn sync_once(&self) -> Result<u32, String> {
        let mut inner = self.inner.lock().await;
        let rep = match inner.scanner.sync(&self.client, None).await {
            Ok(r) => r,
            Err(e) => {
                inner.last_error = Some(e.to_string());
                return Err(e.to_string());
            }
        };
        inner.last_error = None;
        if rep.steps.is_empty() {
            return Ok(0);
        }
        let new: Vec<ChainEvent> = rep.steps.iter().flat_map(|s| self.convert(s)).collect();
        // Drop dev pending entries whose notes are now spent.
        let spent: BTreeSet<_> = inner.scanner.spent_notes().map(|s| s.nullifier).collect();
        inner.dev_pending.retain(|nf, _| !spent.contains(nf));
        let tip = inner.scanner.tip().0;
        let mut log = self.log.write().await;
        let mut seq = log.last_seq;
        let numbered: Vec<(u64, ChainEvent)> = new
            .into_iter()
            .map(|e| {
                seq += 1;
                (seq, e)
            })
            .collect();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
            .map_err(|e| e.to_string())?;
        for ev in &numbered {
            writeln!(f, "{}", serde_json::to_string(ev).unwrap()).map_err(|e| e.to_string())?;
        }
        f.sync_all().map_err(|e| e.to_string())?;
        self.write_state(&inner, seq, tip)?;
        let n = numbered.len() as u32;
        log.events.extend(numbered);
        log.last_seq = seq;
        log.tip = tip;
        Ok(n)
    }

    fn convert(&self, step: &SyncStep) -> Vec<ChainEvent> {
        match step {
            SyncStep::Rewind(h) => vec![ChainEvent::Rewind { to_height: *h }],
            SyncStep::Block(b) => {
                let mut v = vec![];
                for it in &b.items {
                    match it {
                        BlockItem::Received(n)
                            if n.pool == Pool::Ironwood && self.apps.contains_key(&n.app_id) =>
                        {
                            let mut idx = [0u8; 16];
                            idx[..11].copy_from_slice(&n.receiver_index);
                            v.push(ChainEvent::Note(zol_types::ScannedNote {
                                app: n.app_id,
                                cmx: n.cmx,
                                nullifier: n.nullifier,
                                value: n.value,
                                memo: n.memo.to_vec(),
                                receiver: n.receiver.to_vec(),
                                receiver_index: u128::from_le_bytes(idx),
                                scope: match n.scope {
                                    OScope::External => zol_types::Scope::External,
                                    OScope::Internal => zol_types::Scope::Internal,
                                },
                                height: n.height,
                                block_hash: n.block_hash,
                                txid: n.txid,
                            }))
                        }
                        BlockItem::Spent(s) if self.apps.contains_key(&s.app_id) => {
                            v.push(ChainEvent::Spent {
                                app: s.app_id,
                                nullifier: s.nullifier,
                                txid: s.spending_txid,
                                height: s.height,
                            })
                        }
                        _ => {}
                    }
                }
                v.push(ChainEvent::Block {
                    height: b.height,
                    hash: b.hash,
                });
                v
            }
        }
    }
}

// ---------------------------------------------------------------- errors

struct ApiErr(StatusCode, String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(api::Error { error: self.1 })).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::NOT_FOUND, msg.into())
}
fn conflict(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::CONFLICT, msg.into())
}
fn internal(msg: impl ToString) -> ApiErr {
    ApiErr(StatusCode::INTERNAL_SERVER_ERROR, msg.to_string())
}
fn upstream(msg: impl ToString) -> ApiErr {
    ApiErr(StatusCode::BAD_GATEWAY, msg.to_string())
}

type R<T> = Result<Json<T>, ApiErr>;
type S = State<Arc<Node>>;

fn app_of<'a>(n: &'a Node, hexapp: &str) -> Result<&'a AppKeys, ApiErr> {
    let id = hex32(hexapp).map_err(bad)?;
    n.apps.get(&id).ok_or_else(|| not_found("unknown app"))
}

// ---------------------------------------------------------------- handlers

#[derive(Deserialize)]
struct EventsQ {
    after: Option<u64>,
    limit: Option<usize>,
}

async fn events(State(n): S, Query(q): Query<EventsQ>) -> R<api::Events> {
    let after = q.after.unwrap_or(0);
    let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
    let log = n.log.read().await;
    // seqs are 1..=last_seq, contiguous: events[i] has seq i+1.
    let start = (after as usize).min(log.events.len());
    let events: Vec<_> = log.events[start..].iter().take(limit).cloned().collect();
    let next = events
        .last()
        .map(|e| e.0)
        .unwrap_or(after.min(log.last_seq));
    Ok(Json(api::Events {
        events,
        next,
        tip: log.tip,
    }))
}

#[derive(Deserialize)]
struct AppQ {
    app: String,
}

async fn vault(State(n): S, Query(q): Query<AppQ>) -> R<api::Vault> {
    let k = app_of(&n, &q.app)?;
    Ok(Json(api::Vault {
        app: k.app_id,
        address: k.vault_address().to_vec(),
        ua: encode_ua(&n.params, &k.vault_address()),
        internal_address: k.vault_internal_address().to_vec(),
    }))
}

#[derive(Deserialize)]
struct SessionQ {
    app: String,
    session: String,
}

async fn session_receiver(State(n): S, Query(q): Query<SessionQ>) -> R<api::SessionReceiver> {
    let k = app_of(&n, &q.app)?;
    let sess = hex32(&q.session).map_err(bad)?;
    let (r, idx) = k.session_receiver(&sess);
    let mut b = [0u8; 16];
    b[..11].copy_from_slice(&idx);
    Ok(Json(api::SessionReceiver {
        receiver: r.to_vec(),
        index: u128::from_le_bytes(b),
        ua: encode_ua(&n.params, &r),
    }))
}

#[derive(Deserialize)]
struct RawQ {
    raw: String,
}

async fn valid_receiver(Query(q): Query<RawQ>) -> Json<serde_json::Value> {
    let valid = hex::decode(&q.raw)
        .ok()
        .and_then(|b| <[u8; 43]>::try_from(b).ok())
        .map(|b| bool::from(orchard::Address::from_raw_address_bytes(&b).is_some()))
        .unwrap_or(false);
    Json(serde_json::json!({ "valid": valid }))
}

async fn health(State(n): S) -> Json<serde_json::Value> {
    let (tip, err) = {
        let i = n.inner.lock().await;
        (i.scanner.tip().0, i.last_error.clone())
    };
    let log = n.log.read().await;
    Json(serde_json::json!({
        "ok": err.is_none(),
        "mode": n.mode,
        "network": format!("{:?}", n.params.network),
        "scanner_tip": tip,
        "log_tip": log.tip,
        "last_seq": log.last_seq,
        "apps": n.apps.keys().map(hex::encode).collect::<Vec<_>>(),
        "ak_group": hex::encode(n.ak_group),
        "signers_held": n.shares.len(),
        "min_signers": n.min_signers,
        "last_error": err,
        // Dev mode: the address dev funding mines to. A node without `generatetoaddress`
        // (Zakura) must have this as its `[mining] miner_address`.
        "dev_wallet_ua": n.dev.as_ref().map(|d| encode_ua(
            &n.params,
            &d.fvk.address_at(0u32, OScope::External).to_raw_address_bytes(),
        )),
    }))
}

async fn post_refund(State(n): S, Json(req): Json<api::RefundRequest>) -> R<api::RefundResponse> {
    let keys = n
        .apps
        .get(&req.app)
        .ok_or_else(|| not_found("unknown app"))?
        .clone();
    let to: [u8; 43] = req
        .to
        .clone()
        .try_into()
        .map_err(|_| bad("`to` must be 43 bytes"))?;
    if bool::from(orchard::Address::from_raw_address_bytes(&to).is_none()) {
        return Err(bad("`to` is not a valid Orchard-protocol receiver"));
    }
    if (n.shares.len() as u16) < n.min_signers {
        return Err(ApiErr(
            StatusCode::NOT_IMPLEMENTED,
            format!(
                "this process holds {} of {} required FROST shares; distributed signing across operators is not implemented yet (run in cluster/dev mode)",
                n.shares.len(),
                n.min_signers
            ),
        ));
    }
    let _op = n.ops.lock().await;
    let (sp, tip) = {
        let inner = n.inner.lock().await;
        let sc = &inner.scanner;
        let note = sc
            .note_by_cmx(&req.cmx)
            .filter(|x| x.app_id == req.app)
            .ok_or_else(|| not_found("unknown note for this app"))?;
        if note.pool != Pool::Ironwood {
            return Err(bad("not an Ironwood note"));
        }
        if note.scope == OScope::Internal
            || HeadMemo::decode(&note.memo).is_ok()
            || TopUpMemo::decode(&note.memo).is_ok()
        {
            return Err(bad("not a deposit note (internal scope, head or top-up)"));
        }
        let tip = sc.tip().0;
        if let Some(rec) = inner.refunds.get(&req.cmx) {
            match sc.spend_of(&req.cmx) {
                Some(s) if s.spending_txid == rec.txid => {
                    return Ok(Json(api::RefundResponse {
                        txid: rec.txid,
                        value: rec.value,
                        fee: rec.fee,
                        existing: true,
                    }));
                }
                Some(_) => return Err(conflict("note was spent by another transaction")),
                None if tip < rec.expiry_height => {
                    if rec.to != req.to {
                        return Err(conflict(
                            "a refund of this note to a different receiver is pending",
                        ));
                    }
                    return Ok(Json(api::RefundResponse {
                        txid: rec.txid,
                        value: rec.value,
                        fee: rec.fee,
                        existing: true,
                    }));
                }
                None => { /* expired unmined: rebuild below */ }
            }
        } else if sc.spend_of(&req.cmx).is_some() {
            return Err(conflict("note already spent"));
        }
        (sc.spendable(&req.cmx, None).map_err(internal)?, tip)
    };
    let params = n.params;
    let delta = n.expiry_delta;
    let min = n.min_signers as usize;
    let signers: BTreeMap<_, _> = n
        .shares
        .iter()
        .take(min)
        .map(|(i, k)| (*i, k.clone()))
        .collect();
    let pk = n.pubkeys.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let r =
            refund::build(&params, &keys, &sp, &to, tip + 1, delta).map_err(|e| e.to_string())?;
        let (value, fee, exp) = (r.refund_value, r.fee, r.expiry_height);
        let r = refund::sign_with_frost(
            r,
            &keys,
            &sp.note,
            &signers,
            &pk,
            tip,
            &mut rand::rngs::OsRng,
        )
        .map_err(|e| e.to_string())?;
        let (raw, txid) = refund::extract(r, &params).map_err(|e| e.to_string())?;
        Ok((raw, txid, value, fee, exp))
    })
    .await
    .map_err(internal)?
    .map_err(|e| ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let (raw, txid, value, fee, expiry_height) = built;
    n.client
        .send_raw_transaction(&raw)
        .await
        .map_err(upstream)?;
    let mut inner = n.inner.lock().await;
    inner.refunds.insert(
        req.cmx,
        RefundRecord {
            cmx: req.cmx,
            app: req.app,
            to: req.to,
            txid,
            value,
            fee,
            expiry_height,
        },
    );
    let (seq, ltip) = {
        let log = n.log.read().await;
        (log.last_seq, log.tip)
    };
    n.write_state(&inner, seq, ltip).map_err(internal)?;
    Ok(Json(api::RefundResponse {
        txid,
        value,
        fee,
        existing: false,
    }))
}

// ---------------------------------------------------------------- Phase 4: heads, payouts, distributed signing

/// A known, unspent, Ironwood deposit note of `app` (not internal scope, not a head or top-up).
fn deposit_note(
    sc: &Scanner,
    app: &[u8; 32],
    cmx: &[u8; 32],
) -> Result<zol_zcash::ScannedNote, ApiErr> {
    let note = sc
        .note_by_cmx(cmx)
        .filter(|x| &x.app_id == app)
        .ok_or_else(|| not_found("unknown note for this app"))?;
    if note.pool != Pool::Ironwood {
        return Err(bad("not an Ironwood note"));
    }
    if note.scope == OScope::Internal
        || HeadMemo::decode(&note.memo).is_ok()
        || TopUpMemo::decode(&note.memo).is_ok()
    {
        return Err(bad("not a deposit note (internal scope, head or top-up)"));
    }
    if sc.spend_of(cmx).is_some() {
        return Err(conflict("note already spent"));
    }
    Ok(note.clone())
}

fn receiver43(v: &[u8]) -> Result<[u8; 43], ApiErr> {
    let to: [u8; 43] = v
        .to_vec()
        .try_into()
        .map_err(|_| bad("`to` must be 43 bytes"))?;
    if bool::from(orchard::Address::from_raw_address_bytes(&to).is_none()) {
        return Err(bad("`to` is not a valid Orchard-protocol receiver"));
    }
    Ok(to)
}

/// The broadcast transition this daemon knows for `app`'s current head, if still live.
fn pending_for(
    inner: &Inner,
    app: &[u8; 32],
    current: Option<&zol_zcash::head::Link>,
) -> Option<[u8; 32]> {
    let p = inner.pending.get(app)?;
    let live = inner.scanner.tip().0 < p.expiry_height
        && current.is_some_and(|h| h.note.nullifier == p.head_nullifier);
    live.then_some(p.txid)
}

fn unprocessable(msg: impl ToString) -> ApiErr {
    ApiErr(StatusCode::UNPROCESSABLE_ENTITY, msg.to_string())
}

#[derive(Deserialize)]
struct HeadQ {
    app: String,
    genesis: String,
}

async fn get_head(State(n): S, Query(q): Query<HeadQ>) -> R<api::HeadChain> {
    let keys = app_of(&n, &q.app)?;
    let genesis = hex32(&q.genesis).map_err(bad)?;
    let inner = n.inner.lock().await;
    let walk = zol_zcash::head::walk(&inner.scanner, keys, &genesis);
    let pending = pending_for(&inner, &keys.app_id, walk.current());
    Ok(Json(api::HeadChain {
        app: keys.app_id,
        chain: walk.links.iter().map(|l| l.to_api()).collect(),
        pending,
        fault: walk.fault.clone(),
        tip: inner.scanner.tip().0,
    }))
}

async fn payout_propose(State(n): S, Json(plan): Json<api::PayoutPlan>) -> R<api::Proposal> {
    let keys = n
        .apps
        .get(&plan.app)
        .ok_or_else(|| not_found("unknown app"))?;
    let _op = n.ops.lock().await;
    let inner = n.inner.lock().await;
    let (prop, _) = tokio::task::block_in_place(|| {
        payout::propose(
            &n.params,
            keys,
            &inner.scanner,
            &plan,
            &payout::Tamper::default(),
        )
    })
    .map_err(unprocessable)?;
    Ok(Json(prop))
}

async fn payout_verify(State(n): S, Json(req): Json<api::VerifyRequest>) -> R<api::VerifyResponse> {
    let keys = n
        .apps
        .get(&req.plan.app)
        .ok_or_else(|| not_found("unknown app"))?;
    let inner = n.inner.lock().await;
    let walk = zol_zcash::head::walk(&inner.scanner, keys, &req.plan.genesis_head);
    let pending = pending_for(&inner, &keys.app_id, walk.current());
    Ok(Json(tokio::task::block_in_place(|| {
        payout::verify(keys, &inner.scanner, &req.plan, &req.pczt, pending)
    })))
}

async fn refund_propose(
    State(n): S,
    Json(req): Json<api::RefundRequest>,
) -> R<api::RefundProposal> {
    let keys = n
        .apps
        .get(&req.app)
        .ok_or_else(|| not_found("unknown app"))?
        .clone();
    let to = receiver43(&req.to)?;
    let _op = n.ops.lock().await;
    let (sp, tip) = {
        let inner = n.inner.lock().await;
        deposit_note(&inner.scanner, &req.app, &req.cmx)?;
        let tip = inner.scanner.tip().0;
        if let Some(rec) = inner.refunds.get(&req.cmx) {
            if tip < rec.expiry_height {
                return Err(conflict(format!(
                    "a refund of this note is pending: txid {}",
                    hex::encode(rec.txid)
                )));
            }
        }
        (
            inner.scanner.spendable(&req.cmx, None).map_err(internal)?,
            tip,
        )
    };
    let (params, delta) = (n.params, n.expiry_delta);
    let r = tokio::task::spawn_blocking(move || {
        refund::build(&params, &keys, &sp, &to, tip + 1, delta)
    })
    .await
    .map_err(internal)?
    .map_err(unprocessable)?;
    let sighash = zol_zcash::tx::sighash(&r.pczt).map_err(internal)?;
    Ok(Json(api::RefundProposal {
        pczt: r
            .pczt
            .clone()
            .serialize()
            .map_err(|e| internal(format!("{e:?}")))?,
        sighash,
        expiry_height: r.expiry_height,
        value: r.refund_value,
        fee: r.fee,
    }))
}

async fn sign_commit(State(n): S, Json(req): Json<api::CommitRequest>) -> R<api::CommitResponse> {
    let (id, kp) = n.signer.clone().ok_or_else(|| {
        ApiErr(
            StatusCode::NOT_IMPLEMENTED,
            "this daemon holds no FROST share".into(),
        )
    })?;
    let pczt = pczt::Pczt::parse(&req.pczt).map_err(|e| bad(format!("pczt: {e:?}")))?;
    let sighash = zol_zcash::tx::sighash(&pczt).map_err(|e| bad(e.to_string()))?;
    let mut inner = n.inner.lock().await;
    if let Some(s) = inner.sessions.get(&req.session) {
        if s.sighash == sighash && s.nonces.is_some() {
            return Ok(Json(api::CommitResponse {
                identifier: s.identifier.serialize(),
                commitments: s.commitments.clone(),
            }));
        }
        return Err(conflict(
            "session already bound to another transaction, or its nonces were used",
        ));
    }
    let tip = inner.scanner.tip().0;
    let keys = match &req.kind {
        api::SignKind::Payout { plan } => {
            let keys = n
                .apps
                .get(&plan.app)
                .ok_or_else(|| not_found("unknown app"))?;
            let walk = zol_zcash::head::walk(&inner.scanner, keys, &plan.genesis_head);
            let pending = pending_for(&inner, &keys.app_id, walk.current());
            let v = tokio::task::block_in_place(|| {
                payout::verify(keys, &inner.scanner, plan, &req.pczt, pending)
            });
            if !v.ok {
                return Err(unprocessable(format!(
                    "verify failed: {}",
                    v.failures.join(",")
                )));
            }
            keys
        }
        api::SignKind::Refund { app, cmx, to } => {
            let keys = n.apps.get(app).ok_or_else(|| not_found("unknown app"))?;
            let note = deposit_note(&inner.scanner, app, cmx)?;
            let to = receiver43(to)?;
            let pz = &pczt;
            tokio::task::block_in_place(|| refund::check_pczt(pz, keys, &note.note(), &to, tip))
                .map_err(|e| unprocessable(format!("refund check failed: {e}")))?;
            keys
        }
    };
    let actions = payout::vault_spends(keys, &pczt).map_err(internal)?;
    if actions.is_empty() {
        return Err(unprocessable("no vault spend to sign"));
    }
    let (app_kp, _) = frost::app_packages(keys, &kp, &n.pubkeys).map_err(internal)?;
    let mut nonces = BTreeMap::new();
    let mut commitments = vec![];
    for (idx, _) in &actions {
        let (nn, c) = frost::commit(&app_kp, &mut rand::rngs::OsRng);
        nonces.insert(*idx, nn);
        commitments.push((*idx, c.serialize().map_err(|e| internal(format!("{e:?}")))?));
    }
    inner.sessions.insert(
        req.session,
        Session {
            app: keys.app_id,
            sighash,
            identifier: id,
            app_kp,
            actions,
            nonces: Some(nonces),
            commitments: commitments.clone(),
        },
    );
    Ok(Json(api::CommitResponse {
        identifier: id.serialize(),
        commitments,
    }))
}

/// Every signer's commitment for action `idx`.
fn commitments_for(
    list: &[api::CommitResponse],
    idx: u32,
) -> Result<BTreeMap<Identifier, frost::SigningCommitments>, ApiErr> {
    let mut m = BTreeMap::new();
    for c in list {
        let id = Identifier::deserialize(&c.identifier)
            .map_err(|e| bad(format!("identifier: {e:?}")))?;
        let (_, bytes) = c
            .commitments
            .iter()
            .find(|(i, _)| *i == idx)
            .ok_or_else(|| bad(format!("signer missing a commitment for action {idx}")))?;
        m.insert(
            id,
            frost::SigningCommitments::deserialize(bytes)
                .map_err(|e| bad(format!("commitments: {e:?}")))?,
        );
    }
    Ok(m)
}

async fn sign_share(State(n): S, Json(req): Json<api::ShareRequest>) -> R<api::ShareResponse> {
    let mut inner = n.inner.lock().await;
    let s = inner
        .sessions
        .get_mut(&req.session)
        .ok_or_else(|| not_found("unknown session"))?;
    let mine = req
        .commitments
        .iter()
        .find(|c| c.identifier == s.identifier.serialize());
    if mine.map(|c| &c.commitments) != Some(&s.commitments) {
        return Err(conflict(
            "the package does not contain this signer's own round-1 commitments",
        ));
    }
    let nonces = s
        .nonces
        .take()
        .ok_or_else(|| conflict("nonces of this session were already used"))?;
    let mut shares = vec![];
    for (idx, alpha) in s.actions.clone() {
        let comms = commitments_for(&req.commitments, idx)?;
        let sp = frost::SigningPackage::new(comms, &s.sighash);
        let share =
            frost::sign_share(&sp, &nonces[&idx], &s.app_kp, &alpha).map_err(unprocessable)?;
        shares.push((idx, share.serialize()));
    }
    let identifier = s.identifier.serialize();
    Ok(Json(api::ShareResponse { identifier, shares }))
}

async fn sign_aggregate(State(n): S, Json(req): Json<api::AggregateRequest>) -> R<api::Broadcast> {
    let mut pczt = pczt::Pczt::parse(&req.pczt).map_err(|e| bad(format!("pczt: {e:?}")))?;
    let sighash = zol_zcash::tx::sighash(&pczt).map_err(|e| bad(e.to_string()))?;
    // The app: from this daemon's own session if it signed, else the app whose key the
    // spends' rk follow from.
    let sess_app = n
        .inner
        .lock()
        .await
        .sessions
        .get(&req.session)
        .map(|s| s.app);
    let keys = match sess_app.and_then(|a| n.apps.get(&a)) {
        Some(k) => k.clone(),
        None => {
            let mut owners = n.apps.values().filter(|k| {
                payout::vault_spends(k, &pczt)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
            });
            let k = owners
                .next()
                .ok_or_else(|| bad("no vault spend of a configured app"))?
                .clone();
            if owners.next().is_some() {
                return Err(bad("spends of several apps"));
            }
            k
        }
    };
    let app_pk = frost::tweak_public_package(&n.pubkeys, &keys.tweak).map_err(internal)?;
    let spends = payout::vault_spends(&keys, &pczt).map_err(internal)?;
    for (idx, alpha) in &spends {
        let comms = commitments_for(&req.commitments, *idx)?;
        let mut shares = BTreeMap::new();
        for sr in &req.shares {
            let id = Identifier::deserialize(&sr.identifier)
                .map_err(|e| bad(format!("identifier: {e:?}")))?;
            let (_, b) = sr
                .shares
                .iter()
                .find(|(i, _)| i == idx)
                .ok_or_else(|| bad(format!("missing share for action {idx}")))?;
            shares.insert(
                id,
                frost::SignatureShare::deserialize(b).map_err(|e| bad(format!("share: {e:?}")))?,
            );
        }
        let sp = frost::SigningPackage::new(comms, &sighash);
        let sig = frost::aggregate(&sp, &shares, &app_pk, alpha).map_err(unprocessable)?;
        pczt = zol_zcash::tx::apply_signature(pczt, *idx as usize, sig).map_err(unprocessable)?;
    }
    let expiry_height = *pczt.global().expiry_height();
    // What this transaction spends and pays, for the pending records.
    let (nfs, value_out, fee) = zol_zcash::tx::with_ironwood(&pczt, |b| {
        let nfs: Vec<[u8; 32]> = spends
            .iter()
            .map(|(i, _)| b.actions()[*i as usize].spend().nullifier().to_bytes())
            .collect();
        let out = b
            .actions()
            .iter()
            .filter_map(|a| {
                let r = (*a.output().recipient())?;
                let v = (*a.output().value())?.inner();
                (v > 0 && keys.fvk().scope_for_address(&r).is_none())
                    .then(|| (r.to_raw_address_bytes().to_vec(), v))
            })
            .next();
        (nfs, out, b.value_sum().magnitude_sign().0)
    })
    .map_err(internal)?;
    let params = n.params;
    let (raw, txid) = tokio::task::spawn_blocking(move || zol_zcash::tx::extract(pczt, &params))
        .await
        .map_err(internal)?
        .map_err(unprocessable)?;
    n.client
        .send_raw_transaction(&raw)
        .await
        .map_err(upstream)?;
    let mut inner = n.inner.lock().await;
    for nf in nfs {
        let Some(note) = inner
            .scanner
            .all_notes()
            .find(|x| x.nullifier == nf && x.app_id == keys.app_id)
            .cloned()
        else {
            continue;
        };
        if note.scope == OScope::Internal && HeadMemo::decode(&note.memo).is_ok() {
            inner.pending.insert(
                keys.app_id,
                PendingTx {
                    txid,
                    expiry_height,
                    head_nullifier: nf,
                },
            );
        } else if let Some((to, value)) = value_out.clone() {
            if spends.len() == 1 {
                inner.refunds.insert(
                    note.cmx,
                    RefundRecord {
                        cmx: note.cmx,
                        app: keys.app_id,
                        to,
                        txid,
                        value,
                        fee,
                        expiry_height,
                    },
                );
            }
        }
    }
    let (seq, ltip) = {
        let log = n.log.read().await;
        (log.last_seq, log.tip)
    };
    n.write_state(&inner, seq, ltip).map_err(internal)?;
    Ok(Json(api::Broadcast {
        txid,
        expiry_height,
    }))
}

/// Most dev wallet notes one dev transaction spends (bounds proof size and time).
const MAX_DEV_INPUTS: usize = 40;
/// Most coinbase blocks one dev request mines to fund itself.
const MAX_DEV_BLOCKS: u32 = 200;

/// Selects free (unspent, not used by an unmined dev transaction) dev wallet notes, largest
/// first, covering `amount` plus the ZIP 317 fee for the resulting action count (inputs vs.
/// 2 outputs). If the free notes do not cover it, mines coinbase blocks with `generate`
/// (sized from `getblocksubsidy`; this also mines whatever is in the mempool) and retries, up to `MAX_DEV_BLOCKS` blocks and `MAX_DEV_INPUTS` inputs.
/// The coinbase goes to the node's `[mining] miner_address`, which must be the dev wallet's
/// UA (infra/zakura sets the one for dev seed "zol-dev"). All inputs are witnessed at the same
/// anchor (the scanner tip).
async fn dev_inputs(
    n: &Node,
    amount: u64,
) -> Result<(Vec<zol_zcash::scan::Spendable>, u32), ApiErr> {
    let dev = n.dev.as_ref().ok_or_else(|| not_found("dev mode only"))?;
    let mut mined = 0u32;
    loop {
        let (deficit, free_sum, free_n) = {
            let inner = n.inner.lock().await;
            let tip = inner.scanner.tip().0;
            let mut free: Vec<_> = inner
                .scanner
                .unspent(&DEV_WALLET_ID)
                .filter(|x| x.pool == Pool::Ironwood && x.value > 0)
                .filter(|x| {
                    inner
                        .dev_pending
                        .get(&x.nullifier)
                        .is_none_or(|exp| tip >= *exp)
                })
                .map(|x| (x.value, x.cmx))
                .collect();
            free.sort_by(|a, b| b.cmp(a));
            free.truncate(MAX_DEV_INPUTS);
            let mut sum = 0u64;
            let mut take = vec![];
            for (v, cmx) in &free {
                if sum >= amount + testing::zip317_fee(take.len(), 2) {
                    break;
                }
                sum += v;
                take.push(*cmx);
            }
            if !take.is_empty() && sum >= amount + testing::zip317_fee(take.len(), 2) {
                let sps = take
                    .iter()
                    .map(|c| inner.scanner.spendable(c, Some(tip)))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(internal)?;
                return Ok((sps, tip));
            }
            let total: u64 = free.iter().map(|x| x.0).sum();
            let need = amount + testing::zip317_fee(MAX_DEV_INPUTS, 2);
            (need.saturating_sub(total), total, free.len())
        };
        let subsidy: serde_json::Value = n
            .client
            .call("getblocksubsidy", serde_json::json!([]))
            .await
            .map_err(upstream)?;
        let per_block = (subsidy["miner"].as_f64().unwrap_or(0.0) * 1e8).round() as u64;
        if per_block == 0 {
            return Err(bad(format!(
                "regtest block subsidy is 0: dev wallet cannot fund {amount} zat (free: {free_sum} zat in {free_n} notes); restart the regtest chain"
            )));
        }
        if mined >= MAX_DEV_BLOCKS {
            return Err(bad(format!(
                "dev wallet cannot fund {amount} zat within {MAX_DEV_INPUTS} inputs after mining {mined} blocks (cap {MAX_DEV_BLOCKS}); free: {free_sum} zat in {free_n} notes; block subsidy {per_block} zat. The node's [mining] miner_address must be the dev wallet UA {}: GET /health dev_wallet_ua, or --print-dev-keys",
                encode_ua(&n.params, &dev.fvk.address_at(0u32, OScope::External).to_raw_address_bytes())
            )));
        }
        let blocks = (deficit.div_ceil(per_block) + 1)
            .min(MAX_DEV_INPUTS as u64)
            .min((MAX_DEV_BLOCKS - mined) as u64) as u32;
        n.client.generate(blocks).await.map_err(upstream)?;
        mined += blocks;
        n.sync_once().await.map_err(internal)?;
    }
}

async fn dev_genesis(State(n): S, Json(req): Json<api::GenesisRequest>) -> R<api::GenesisResponse> {
    let keys = n
        .apps
        .get(&req.app)
        .ok_or_else(|| not_found("unknown app"))?
        .clone();
    let _op = n.ops.lock().await;
    let (sps, tip) = dev_inputs(&n, req.float).await?;
    let sk = n.dev.as_ref().unwrap().sk;
    let (params, delta) = (n.params, n.expiry_delta);
    let nfs: Vec<_> = sps.iter().map(|sp| sp.scanned.nullifier).collect();
    let g = tokio::task::spawn_blocking(move || {
        genesis::build_multi(&params, &sk, &sps, &keys, req.float, tip + 1, delta)
    })
    .await
    .map_err(internal)?
    .map_err(|e| ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    n.client
        .send_raw_transaction(&g.raw_tx)
        .await
        .map_err(upstream)?;
    let mut inner = n.inner.lock().await;
    for nf in nfs {
        inner.dev_pending.insert(nf, tip + 1 + delta);
    }
    drop(inner);
    Ok(Json(api::GenesisResponse {
        head_cmx: g.head_cmx,
        txid: g.txid,
        head_value: g.head_value,
    }))
}

async fn dev_send(State(n): S, Json(req): Json<api::SendRequest>) -> R<api::SendResponse> {
    if n.dev.is_none() {
        return Err(not_found("dev mode only"));
    }
    let to: [u8; 43] = req
        .to
        .clone()
        .try_into()
        .map_err(|_| bad("`to` must be 43 bytes"))?;
    if req.memo.len() > 512 {
        return Err(bad("memo longer than 512 bytes"));
    }
    let mut memo = [0u8; 512];
    memo[..req.memo.len()].copy_from_slice(&req.memo);
    if req.memo.is_empty() {
        memo[0] = 0xF6;
    }
    let _op = n.ops.lock().await;
    let (sps, tip) = dev_inputs(&n, req.value).await?;
    let sk = n.dev.as_ref().unwrap().sk;
    let params = n.params;
    let nfs: Vec<_> = sps.iter().map(|sp| sp.scanned.nullifier).collect();
    let value = req.value;
    let (raw, txid) = tokio::task::spawn_blocking(move || {
        let inputs: Vec<_> = sps.iter().map(|sp| (sp.note, sp.path.clone())).collect();
        testing::wallet_send_multi(
            &params,
            &sk,
            &inputs,
            sps[0].anchor,
            &to,
            value,
            memo,
            tip + 1,
        )
    })
    .await
    .map_err(internal)?
    .map_err(|e| ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    n.client
        .send_raw_transaction(&raw)
        .await
        .map_err(upstream)?;
    let mut inner = n.inner.lock().await;
    for nf in nfs {
        inner
            .dev_pending
            .insert(nf, tip + 1 + zol_zcash::DEFAULT_EXPIRY_DELTA);
    }
    drop(inner);
    Ok(Json(api::SendResponse { txid }))
}

// ---------------------------------------------------------------- startup

struct Keys {
    master_view: [u8; 32],
    shares: BTreeMap<Identifier, KeyPackage>,
    pubkeys: PublicKeyPackage,
}

fn dev_keys(seed: &str) -> (Keys, SpendingKey) {
    let k = testing::dev_seed_keys(seed);
    (
        Keys {
            master_view: k.master_view,
            shares: k.shares,
            pubkeys: k.pubkeys,
        },
        k.wallet,
    )
}

fn config_keys(k: &KeysCfg) -> Result<Keys, String> {
    let pubkeys = PublicKeyPackage::deserialize(
        &hex::decode(&k.public_key_package).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("public_key_package: {e:?}"))?;
    let mut shares = BTreeMap::new();
    for s in &k.shares {
        let kp = KeyPackage::deserialize(&hex::decode(s).map_err(|e| e.to_string())?)
            .map_err(|e| format!("share: {e:?}"))?;
        if kp.verifying_key() != pubkeys.verifying_key() {
            return Err("share does not belong to public_key_package".into());
        }
        shares.insert(*kp.identifier(), kp);
    }
    Ok(Keys {
        master_view: hex32(&k.master_view)?,
        shares,
        pubkeys,
    })
}

fn load_log(path: &Path, last_seq: u64) -> Result<Vec<(u64, ChainEvent)>, String> {
    let mut v = vec![];
    if path.exists() {
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        for line in std::io::BufReader::new(f).lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<(u64, ChainEvent)>(&line) {
                Ok(e) if e.0 <= last_seq => v.push(e),
                Ok(_) => break,  // written but never committed by state.bin: drop
                Err(_) => break, // torn last line
            }
        }
    }
    for (i, (s, _)) in v.iter().enumerate() {
        if *s != i as u64 + 1 {
            return Err(format!("event log is not contiguous at line {i}"));
        }
    }
    if v.len() as u64 != last_seq {
        return Err(format!(
            "event log has {} events, state says {last_seq}",
            v.len()
        ));
    }
    // Rewrite to drop any uncommitted tail.
    let tmp = path.with_extension("jsonl.tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    for e in &v {
        writeln!(f, "{}", serde_json::to_string(e).unwrap()).map_err(|e| e.to_string())?;
    }
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(v)
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("zol-zcashd: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let dev_mode = args.iter().any(|a| a == "--dev");
    if args.iter().any(|a| a == "--print-dev-keys") {
        // Prints a `[keys]` section (cluster mode: all 3 shares) derived from the dev seed,
        // e.g. to see the format or to pin a dev key set in a config file.
        let seed = arg("--dev-seed").unwrap_or_else(|| "zol-dev".into());
        let (k, _) = dev_keys(&seed);
        println!("# ak_group = {}", hex::encode(frost::group_key(&k.pubkeys)));
        println!(
            "# dev_wallet_ua (regtest [mining] miner_address) = {}",
            testing::dev_wallet_ua(&ChainParams::new(Network::Regtest), &seed)
        );
        println!("[keys]");
        println!("master_view = \"{}\"", hex::encode(k.master_view));
        println!(
            "public_key_package = \"{}\"",
            hex::encode(k.pubkeys.serialize().map_err(|e| format!("{e:?}"))?)
        );
        let shares: Vec<String> = k
            .shares
            .values()
            .map(|kp| format!("\"{}\"", hex::encode(kp.serialize().unwrap())))
            .collect();
        println!("shares = [\n  {},\n]", shares.join(",\n  "));
        return Ok(());
    }
    let cfg_path = arg("--config").ok_or(
        "usage: zol-zcashd --config <file.toml> [--dev [--dev-seed <s>] [--dev-share <i>]]",
    )?;
    let cfg: Config = toml::from_str(
        &std::fs::read_to_string(&cfg_path).map_err(|e| format!("{cfg_path}: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let network = match cfg.network.as_str() {
        "mainnet" => Network::Mainnet,
        "testnet" => Network::Testnet,
        "regtest" => Network::Regtest,
        other => return Err(format!("unknown network {other}")),
    };
    let mut params = ChainParams::new(network);
    if dev_mode && network != Network::Regtest {
        return Err("--dev is only allowed on regtest".into());
    }
    let (keys, dev) = match (&cfg.keys, dev_mode) {
        (Some(k), dev) => {
            let wallet =
                dev.then(|| dev_keys(&arg("--dev-seed").unwrap_or_else(|| "zol-dev".into())).1);
            (config_keys(k)?, wallet)
        }
        (None, true) => {
            let (k, w) = dev_keys(&arg("--dev-seed").unwrap_or_else(|| "zol-dev".into()));
            (k, Some(w))
        }
        (None, false) => return Err("`keys` is required without --dev".into()),
    };
    let mut keys = keys;
    if let Some(i) = arg("--dev-share") {
        if dev.is_none() {
            return Err("--dev-share needs --dev".into());
        }
        let i: usize = i.parse().map_err(|_| "--dev-share <index>")?;
        let (id, kp) = keys
            .shares
            .iter()
            .nth(i)
            .map(|(a, b)| (*a, b.clone()))
            .ok_or("--dev-share index out of range")?;
        keys.shares = BTreeMap::from([(id, kp)]);
    }
    let ak_group = frost::group_key(&keys.pubkeys);
    let min_signers = keys.pubkeys.min_signers().unwrap_or(2);
    let mut apps = BTreeMap::new();
    for a in &cfg.apps {
        let id = hex32(a)?;
        if id == DEV_WALLET_ID {
            return Err("app id collides with the dev wallet id".into());
        }
        apps.insert(
            id,
            AppKeys::derive(ak_group, &keys.master_view, &id, network)
                .map_err(|e| e.to_string())?,
        );
    }
    let signer = keys.shares.iter().next().map(|(a, b)| (*a, b.clone()));
    let mode = if dev.is_some() {
        if keys.shares.len() as u16 >= min_signers {
            "dev"
        } else {
            "dev-operator"
        }
    } else if keys.shares.len() as u16 >= min_signers {
        "cluster"
    } else {
        "operator"
    };
    let mut client = NodeClient::new(cfg.node.url.clone());
    if let (Some(u), Some(p)) = (&cfg.node.user, &cfg.node.password) {
        client = client.with_basic_auth(u.clone(), p.clone());
    }
    std::fs::create_dir_all(&cfg.state_dir).map_err(|e| e.to_string())?;

    let mut accounts: Vec<Account> = apps.values().map(Account::from).collect();
    let dev = dev.map(|sk| Dev {
        fvk: FullViewingKey::from(&sk),
        sk,
    });
    if let Some(d) = &dev {
        accounts.push(Account {
            id: DEV_WALLET_ID,
            fvk: d.fvk.clone(),
        });
    }

    // Wait for the node.
    let mut tries = 0;
    let node_tip = loop {
        match client.get_block_count().await {
            Ok(h) => break h,
            Err(e) if tries < 60 => {
                tries += 1;
                eprintln!("waiting for the node: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    // A pruning node (Zakura) only serves blocks and tree states from `pruneheight` on;
    // the scanner cannot skip blocks, so refuse to start rather than fail every sync.
    let info = client
        .get_blockchain_info()
        .await
        .map_err(|e| e.to_string())?;
    let prune_height = info.prune_height().unwrap_or(0);
    // Regtest activation heights are node configuration: take them from the node, so the
    // same binary builds NU6.3 transactions on a pre-NU7 regtest and NU7 ones on a NU7 one.
    if network == Network::Regtest {
        params = ChainParams::regtest_from_upgrades(
            info.rest
                .get("upgrades")
                .unwrap_or(&serde_json::Value::Null),
        )?;
        eprintln!(
            "regtest activation heights from the node: {:?}",
            params.regtest
        );
    }
    let check_prune = |need: u32, what: &str| {
        if need < prune_height {
            Err(format!(
                "the node is pruned below height {prune_height} but {what} needs block {need}: \
                 point [node] at an archive node or one whose retention covers it \
                 (docs/ops/zcash-node.md)"
            ))
        } else {
            Ok(())
        }
    };

    let state_path = cfg.state_dir.join("state.bin");
    let log_path = cfg.state_dir.join("events.jsonl");
    let (scanner, refunds, pending, log) = if state_path.exists() {
        let p: Persisted =
            bincode::deserialize(&std::fs::read(&state_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if p.version != STATE_VERSION {
            return Err(format!("state version {}", p.version));
        }
        let ids = Scanner::snapshot_account_ids(&p.scanner).map_err(|e| e.to_string())?;
        let (old, new): (Vec<Account>, Vec<Account>) =
            accounts.into_iter().partition(|a| ids.contains(&a.id));
        let mut sc = Scanner::restore(&p.scanner, old)
            .map_err(|e| format!("restore (removed apps are not supported): {e}"))?;
        check_prune(sc.next_height(), "resuming the scanner")?;
        for a in new {
            eprintln!(
                "new app {}: scanned from height {} on",
                hex::encode(a.id),
                sc.next_height()
            );
            sc.add_account(a);
        }
        let events = load_log(&log_path, p.last_seq)?;
        let refunds = p.refunds.into_iter().map(|r| (r.cmx, r)).collect();
        let pending = p.pending.into_iter().collect();
        (
            sc,
            refunds,
            pending,
            Log {
                events,
                last_seq: p.last_seq,
                tip: p.log_tip,
            },
        )
    } else {
        let start = cfg.start_height.unwrap_or(node_tip + 1).max(1);
        check_prune(start - 1, "start_height (tree state at start_height - 1)")?;
        let sc = Scanner::from_node(ScannerConfig::new(params), accounts, &client, start)
            .await
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&log_path);
        let tip = sc.tip().0;
        (
            sc,
            BTreeMap::new(),
            BTreeMap::new(),
            Log {
                events: vec![],
                last_seq: 0,
                tip,
            },
        )
    };
    let node = Arc::new(Node {
        params,
        expiry_delta: cfg.expiry_delta,
        state_dir: cfg.state_dir.clone(),
        client,
        apps,
        shares: keys.shares,
        signer,
        pubkeys: keys.pubkeys,
        min_signers,
        ak_group,
        mode,
        dev,
        inner: Mutex::new(Inner {
            scanner,
            refunds,
            dev_pending: BTreeMap::new(),
            last_error: None,
            pending,
            sessions: BTreeMap::new(),
        }),
        log: RwLock::new(log),
        ops: Mutex::new(()),
    });
    {
        let inner = node.inner.lock().await;
        let log = node.log.read().await;
        node.write_state(&inner, log.last_seq, log.tip)?;
    }

    let bg = node.clone();
    let poll = Duration::from_millis(cfg.poll_ms);
    tokio::spawn(async move {
        loop {
            if let Err(e) = bg.sync_once().await {
                eprintln!("sync: {e}");
            }
            tokio::time::sleep(poll).await;
        }
    });

    let mut app = Router::new()
        .route("/events", get(events))
        .route("/vault", get(vault))
        .route("/session_receiver", get(session_receiver))
        .route("/valid_receiver", get(valid_receiver))
        .route("/refund", post(post_refund))
        .route("/refund/propose", post(refund_propose))
        .route("/head", get(get_head))
        .route("/payout/propose", post(payout_propose))
        .route("/payout/verify", post(payout_verify))
        .route("/sign/commit", post(sign_commit))
        .route("/sign/share", post(sign_share))
        .route("/sign/aggregate", post(sign_aggregate))
        .route("/health", get(health));
    if node.dev.is_some() {
        app = app
            .route("/dev/genesis", post(dev_genesis))
            .route("/dev/send", post(dev_send));
    }
    let app = app.with_state(node.clone());
    let addr: SocketAddr = cfg.listen.parse().map_err(|e| format!("listen: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| e.to_string())?;
    eprintln!(
        "zol-zcashd {mode} on {addr}: network {network:?}, {} apps, ak_group {}, scanner tip {}",
        node.apps.len(),
        hex::encode(ak_group),
        node.inner.lock().await.scanner.tip().0
    );
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}
