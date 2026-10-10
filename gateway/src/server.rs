//! The gateway's HTTP server.
//!
//! - `/peer/*`: other operators' coordinators (authenticated; see `peer`).
//! - `/v1/*`: app pages (public, CORS open; see `zol_types::page`).
//! - `/health`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anchor_lang::prelude::Pubkey;
use ed25519_dalek::VerifyingKey;
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response};
use zol_ledger::state::AppState;
use zol_types::page;
use zol_wire::{DepositMemo, MEMO_LEN};

use crate::ledger::Ledger;
use crate::peer::{verify_request, AttestReq, BroadcastNotice, HttpPeer, IntentNotice, Peer, PeerError, SharedPeer, SpendCommitReq, SpendShareReq};
use crate::store::{NoteKind, NoteState};

/// Replay cache: (peer key, nonce) → timestamp.
pub type ReplayCache = Mutex<HashMap<([u8; 32], [u8; 16]), i64>>;

/// Lock order: `ledger` before the operator (`local`), everywhere.
pub struct ServerState {
    pub local: SharedPeer,
    /// Read-only ledger view for answering peers and pages.
    pub ledger: Mutex<Box<dyn Ledger + Send>>,
    /// Guardian keys of the operators allowed to call `/peer/*`.
    pub peers: Vec<VerifyingKey>,
    pub seen: ReplayCache,
    /// Where to pass on intents that pages register here: the other operators
    /// (name, peer URL) and this host's guardian seed to sign with.
    pub gossip: Vec<(String, String)>,
    pub gossip_seed: [u8; 32],
    /// Every operator's public page API base URL, for `AppInfo.operators`.
    pub operators: Vec<String>,
}

/// Passes an intent to every other operator in the background, retrying a few
/// times. A page that wants certainty registers with each operator itself.
fn gossip_intent(st: &ServerState, app: [u8; 32], memo: Vec<u8>) {
    for (name, url) in st.gossip.clone() {
        let (seed, memo) = (st.gossip_seed, memo.clone());
        std::thread::spawn(move || {
            let peer = HttpPeer::new(&name, &url, &seed);
            for wait in [1, 4, 16, 64] {
                match peer.send_intent(&app, &memo) {
                    Ok(()) => return,
                    // Refused (bad memo, unknown app): retrying won't help.
                    Err(PeerError::Refused(e)) => {
                        eprintln!("intent to {name} refused: {e}");
                        return;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_secs(wait)),
                }
            }
            eprintln!("intent to {name}: unreachable, gave up");
        });
    }
}

type Resp = Response<std::io::Cursor<Vec<u8>>>;

fn json<T: Serialize>(code: u16, v: &T) -> Resp {
    let body = serde_json::to_vec(v).unwrap_or_default();
    Response::from_data(body)
        .with_status_code(code)
        .with_header(Header::from_bytes("content-type", "application/json").unwrap())
        .with_header(Header::from_bytes("access-control-allow-origin", "*").unwrap())
        .with_header(Header::from_bytes("access-control-allow-headers", "content-type").unwrap())
        .with_header(Header::from_bytes("access-control-allow-methods", "GET, POST, OPTIONS").unwrap())
}

fn err(code: u16, msg: impl ToString) -> Resp {
    json(code, &serde_json::json!({ "error": msg.to_string() }))
}

fn peer_err(e: PeerError) -> Resp {
    json(409, &e)
}

fn header<'a>(r: &'a Request, name: &'static str) -> &'a str {
    r.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str()).unwrap_or("")
}

/// Starts the server on `addr` in background threads.
pub fn spawn(addr: &str, state: Arc<ServerState>) -> std::io::Result<Arc<tiny_http::Server>> {
    let server = Arc::new(tiny_http::Server::http(addr).map_err(|e| std::io::Error::other(e.to_string()))?);
    for _ in 0..4 {
        let s = server.clone();
        let st = state.clone();
        std::thread::spawn(move || {
            for mut req in s.incoming_requests() {
                let mut body = Vec::new();
                let _ = req.as_reader().read_to_end(&mut body);
                let resp = handle(&st, &req, &body);
                let _ = req.respond(resp);
            }
        });
    }
    Ok(server)
}

fn handle(st: &ServerState, req: &Request, body: &[u8]) -> Resp {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    if *req.method() == Method::Options {
        return json(204, &());
    }
    if path == "/health" {
        let halted = st.local.0.lock().ok().and_then(|g| g.node.halted().ok().flatten());
        return json(200, &serde_json::json!({ "ok": true, "name": st.local.name(), "halted": halted }));
    }
    if let Some(m) = path.strip_prefix("/peer/") {
        if *req.method() != Method::Post {
            return err(405, "POST only");
        }
        let auth = {
            let mut seen = st.seen.lock().unwrap();
            verify_request(
                &st.peers,
                &mut seen,
                &path,
                crate::peer::AuthHeaders {
                    peer: header(req, "x-zol-peer"),
                    ts: header(req, "x-zol-ts"),
                    nonce: header(req, "x-zol-nonce"),
                    sig: header(req, "x-zol-sig"),
                },
                body,
                crate::peer::now_secs(),
            )
        };
        if let Err(e) = auth {
            return json(401, &e);
        }
        return peer_call(st, m, body);
    }
    if let Some(rest) = path.strip_prefix("/v1/apps/") {
        return page_call(st, req.method(), rest, body);
    }
    err(404, "not found")
}

fn peer_call(st: &ServerState, method: &str, body: &[u8]) -> Resp {
    let ledger = st.ledger.lock().unwrap();
    let ledger: &dyn Ledger = ledger.as_ref();
    let mut local = st.local.clone();
    let now = crate::peer::now_secs();
    macro_rules! parse {
        ($t:ty) => {
            match serde_json::from_slice::<$t>(body) {
                Ok(v) => v,
                Err(e) => return err(400, e),
            }
        };
    }
    match method {
        "chain_report" => match local.chain_report() {
            Ok(v) => json(200, &v),
            Err(e) => peer_err(e),
        },
        "attest_commit" => {
            let r = parse!(AttestReq);
            match local.attest_commit(ledger, r.session, &r.statement) {
                Ok(v) => json(200, &v),
                Err(e) => peer_err(e),
            }
        }
        "attest_sign" => {
            let r = parse!(AttestReq);
            let Some(pkg) = r.package else { return err(400, "package missing") };
            match local.attest_sign(ledger, r.session, &r.statement, &pkg) {
                Ok(v) => json(200, &v),
                Err(e) => peer_err(e),
            }
        }
        "spend_commit" => {
            let r = parse!(SpendCommitReq);
            match local.spend_commit(ledger, r.session, &r.target, &r.pczt, now) {
                Ok(v) => json(200, &v),
                Err(e) => peer_err(e),
            }
        }
        "spend_share" => {
            let r = parse!(SpendShareReq);
            match local.spend_share(r.session, &r.commitments) {
                Ok(v) => json(200, &v),
                Err(e) => peer_err(e),
            }
        }
        "intent" => {
            let r = parse!(IntentNotice);
            let Ok(m) = <[u8; MEMO_LEN]>::try_from(r.memo.as_slice()) else { return err(400, "memo must be 512 bytes") };
            let g = st.local.0.lock().unwrap();
            match g.node.register_intent(&r.app, &m) {
                Ok(_) => json(200, &()),
                Err(e) => peer_err(PeerError::Refused(e.to_string())),
            }
        }
        "note_broadcast" => {
            let r = parse!(BroadcastNotice);
            match local.note_broadcast(ledger, &r, now) {
                Ok(()) => json(200, &()),
                Err(e) => peer_err(e),
            }
        }
        _ => err(404, "unknown peer method"),
    }
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    hex::decode(s).ok()?.try_into().ok()
}

fn page_call(st: &ServerState, method: &Method, rest: &str, body: &[u8]) -> Resp {
    let parts: Vec<&str> = rest.split('/').collect();
    let Ok(program) = parts[0].parse::<Pubkey>() else { return err(400, "app must be a base58 program id") };
    let app = program.to_bytes();
    // Lock order everywhere: ledger before operator. Read what the ledger is
    // needed for first and release it.
    let (app_state, denominations) = if matches!((method, &parts[1..]), (Method::Get, [])) {
        let l = st.ledger.lock().unwrap();
        let d = l.config().map(|c| c.params.denominations.iter().copied().filter(|d| *d > 0).collect()).unwrap_or_default();
        // The ledger turns Activating into Active lazily (`App::refresh`, on the next
        // instruction); report what it would be now, as the gateways decide by it.
        let now = l.now().unwrap_or(0);
        (l.app(&program).ok().flatten().map(|a| if a.state == AppState::Activating && now >= a.activates_at { AppState::Active } else { a.state }), d)
    } else {
        (None, Vec::new())
    };
    let guard = st.local.0.lock().unwrap();
    let node = &guard.node;
    let Some(rt) = node.apps.get(&app) else { return err(404, "unknown app") };
    match (method, &parts[1..]) {
        (Method::Get, []) => {
            let state = match app_state {
                Some(AppState::Pending) => "pending",
                Some(AppState::Activating) => "activating",
                Some(AppState::Active) => "active",
                Some(AppState::Frozen) => "frozen",
                None => "unregistered",
            };
            let tip = node.store.tip().ok().flatten().unwrap_or(0);
            json(
                200,
                &page::AppInfo {
                    app: program.to_string(),
                    network: match node.cfg.network() {
                        zol_wire::Network::Mainnet => "mainnet",
                        zol_wire::Network::Testnet => "testnet",
                        zol_wire::Network::Regtest => "regtest",
                    }
                    .into(),
                    vault_address: hex::encode(rt.vault_address),
                    denominations,
                    tip,
                    // In blocks at the current tip, as the page counts them.
                    early_depth: node.cfg.rule().blocks(node.cfg.depths.early, tip),
                    full_depth: node.cfg.rule().blocks(node.cfg.depths.full, tip),
                    state: state.into(),
                    operators: st.operators.clone(),
                },
            )
        }
        (Method::Get, ["sessions", s]) => match hex32(s) {
            Some(session) => session_info(&guard, &app, &session),
            None => err(400, "session must be 32-byte hex"),
        },
        (Method::Post, ["intents"]) => {
            let Ok(i) = serde_json::from_slice::<page::Intent>(body) else { return err(400, "body must be {\"memo\": hex}") };
            let Ok(bytes) = hex::decode(&i.memo) else { return err(400, "memo hex") };
            if bytes.len() > MEMO_LEN {
                return err(400, "memo longer than 512 bytes");
            }
            let mut m = [0u8; MEMO_LEN];
            m[..bytes.len()].copy_from_slice(&bytes);
            match node.register_intent(&app, &m) {
                Ok(_) => {
                    gossip_intent(st, app, m.to_vec());
                    let session = DepositMemo::decode_any(&m).map(|d| d.session_key).unwrap_or([0; 32]);
                    session_info(&guard, &app, &session)
                }
                Err(e) => err(400, e),
            }
        }
        (Method::Get, ["sessions", s, "deposits"]) => {
            let Some(session) = hex32(s) else { return err(400, "session must be 32-byte hex") };
            let idx = (rt.session_index)(&session);
            let tip = node.store.tip().ok().flatten().unwrap_or(0);
            let notes = node.store.notes(&app).unwrap_or_default();
            let out: Vec<page::DepositStatus> = notes
                .into_iter()
                .filter(|n| n.kind == NoteKind::Deposit && !n.rolled_back)
                .filter(|n| {
                    n.note.receiver_index == idx
                        || DepositMemo::decode_any(&crate::chain::memo_array(&n.note)).is_ok_and(|m| m.session_key == session)
                })
                .map(|n| page::DepositStatus {
                    cmx: hex::encode(n.note.cmx),
                    value: n.note.value,
                    height: n.note.height,
                    confirmations: crate::policy::confirmations(n.note.height, tip),
                    state: match (n.state, n.spent.is_some()) {
                        (NoteState::Voided, true) => "refunded",
                        (NoteState::Voided, false) => "voided",
                        (NoteState::Credited, _) => "credited",
                        (NoteState::Held, _) => "held",
                        (NoteState::Seen, _) => "seen",
                    }
                    .into(),
                })
                .collect();
            json(200, &out)
        }
        _ => err(404, "not found"),
    }
}

fn session_info(op: &crate::service::Operator, app: &[u8; 32], session: &[u8; 32]) -> Resp {
    match op.zcashd.session_receiver(app, session) {
        Ok(r) => json(
            200,
            &page::SessionInfo {
                ua: r.ua,
                receiver_index: r.index.to_string(),
                intent_registered: op.node.store.intent(app, r.index).ok().flatten().is_some(),
            },
        ),
        Err(e) => err(502, e),
    }
}
