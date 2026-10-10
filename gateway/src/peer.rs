//! Operators as peers. One interface, three implementations:
//!
//! - [`LocalPeer`]: an operator in this process (its node and its zcashd).
//! - [`SharedPeer`]: this host's own operator behind a mutex (serve mode,
//!   where the HTTP server answers peers concurrently with the service loop).
//! - [`HttpPeer`]: another operator's gateway over authenticated HTTP.
//!
//! Every answer comes from the answering operator's own view: it attests only a
//! statement its own scan and ledger reads produce, and it asks its own zcashd
//! to commit to a spend only with its own payout plan or its own refund list.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use frost_ed25519 as frost;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use zol_types::api::{Broadcast, CommitRequest, CommitResponse, Proposal, RefundProposal, ShareRequest, ShareResponse, SignKind};

use crate::attestor::{AttestError, Statement};
use crate::chain::AppId;
use crate::ledger::Ledger;
use crate::node::{Node, NodeError};
use crate::payout;
use crate::zcashd::ZcashdClient;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum PeerError {
    #[error("refused: {0}")]
    Refused(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("not available on this peer: {0}")]
    NotLocal(String),
}

impl From<NodeError> for PeerError {
    fn from(e: NodeError) -> Self {
        PeerError::Refused(e.to_string())
    }
}

impl From<AttestError> for PeerError {
    fn from(e: AttestError) -> Self {
        PeerError::Refused(e.to_string())
    }
}

pub type PResult<T> = std::result::Result<T, PeerError>;

/// What a spend-signing request asks a peer to sign. The peer derives the
/// actual `SignKind` from its own view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpendTarget {
    Payout { app: AppId },
    Refund { app: AppId, cmx: [u8; 32], to: Vec<u8> },
}

/// A broadcast spend every peer should track as pending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastNotice {
    pub target: SpendTarget,
    pub txid: [u8; 32],
    pub expiry_height: u32,
}

/// The coordinator's proposals for a round, from its own view.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Proposals {
    pub statements: Vec<Statement>,
    pub paid: Vec<Statement>,
}

pub trait Peer {
    fn name(&self) -> String;

    // ---------------------------------------------------------------- every peer
    fn chain_report(&mut self) -> PResult<Option<(u32, [u8; 32])>>;
    /// FROST round 1 for an Ed25519 attestation, only if this peer's own view
    /// gives the same statement and its log holds no conflicting decision.
    /// Returns (identifier, commitments), both serialized.
    fn attest_commit(&mut self, ledger: &dyn Ledger, session: u64, st: &Statement) -> PResult<(Vec<u8>, Vec<u8>)>;
    /// FROST round 2; records the decision in the log before releasing the share.
    fn attest_sign(&mut self, ledger: &dyn Ledger, session: u64, st: &Statement, package: &[u8]) -> PResult<(Vec<u8>, Vec<u8>)>;
    /// Pallas FROST round 1 through this peer's own zcashd, with its own plan.
    fn spend_commit(&mut self, ledger: &dyn Ledger, session: u64, target: &SpendTarget, pczt: &[u8], now: i64) -> PResult<CommitResponse>;
    fn spend_share(&mut self, session: u64, commitments: &[CommitResponse]) -> PResult<ShareResponse>;
    fn note_broadcast(&mut self, ledger: &dyn Ledger, notice: &BroadcastNotice, now: i64) -> PResult<()>;
    /// Bring local state in line with the ledger (decisions others relayed).
    /// Remote peers do this in their own loop.
    fn sync(&mut self, ledger: &dyn Ledger) -> PResult<()>;

    // ---------------------------------------------------------------- coordinator only
    fn proposals(&mut self, ledger: &dyn Ledger) -> PResult<Proposals>;
    fn payout_proposal(&mut self, ledger: &dyn Ledger, app: &AppId, now: i64) -> PResult<Option<(payout::Planned, Proposal)>>;
    fn refund_proposals(&mut self, ledger: &dyn Ledger) -> PResult<Vec<(SpendTarget, RefundProposal)>>;
    fn aggregate(&mut self, session: u64, pczt: &[u8], commitments: Vec<CommitResponse>, shares: Vec<ShareResponse>) -> PResult<Broadcast>;
    fn app_ids(&self) -> Vec<AppId>;
    fn program(&self, app: &AppId) -> Option<anchor_lang::prelude::Pubkey>;
    fn wait_reason(&mut self, ledger: &dyn Ledger, app: &AppId, now: i64) -> Option<payout::Wait>;
}

/// An operator in this process.
pub struct LocalPeer<'a> {
    pub node: &'a mut Node,
    pub zcashd: Option<&'a ZcashdClient>,
}

fn own_statement(node: &Node, ledger: &dyn Ledger, st: &Statement) -> Option<Statement> {
    match st {
        Statement::Credit { app, .. } | Statement::Void { app, .. } => node
            .own_statements(ledger, app)
            .ok()?
            .into_iter()
            .map(|(_, s, _)| s)
            .find(|s| s.log_key() == st.log_key()),
        Statement::Paid { app, .. } => node.own_paid_statements(ledger, app).ok()?.into_iter().find(|s| s == st),
    }
}

impl LocalPeer<'_> {
    fn zcashd(&self) -> PResult<&ZcashdClient> {
        self.zcashd.ok_or_else(|| PeerError::NotLocal("no zcashd".into()))
    }

    fn own_kind(&self, ledger: &dyn Ledger, target: &SpendTarget, now: i64) -> PResult<SignKind> {
        match target {
            SpendTarget::Payout { app } => match payout::plan(self.node, ledger, app, now)? {
                Ok(p) => Ok(SignKind::Payout { plan: p.plan }),
                Err(w) => Err(PeerError::Refused(format!("own plan waits: {w:?}"))),
            },
            SpendTarget::Refund { app, cmx, to } => {
                let due = self.node.refunds_due(ledger, app)?;
                if due.iter().any(|(n, t)| n.note.cmx == *cmx && t.as_slice() == to.as_slice()) {
                    Ok(SignKind::Refund { app: *app, cmx: *cmx, to: to.clone() })
                } else {
                    Err(PeerError::Refused("refund not due in own view".into()))
                }
            }
        }
    }
}

impl Peer for LocalPeer<'_> {
    fn name(&self) -> String {
        self.node.name.clone()
    }

    fn chain_report(&mut self) -> PResult<Option<(u32, [u8; 32])>> {
        Ok(self.node.chain_report()?)
    }

    fn attest_commit(&mut self, ledger: &dyn Ledger, session: u64, st: &Statement) -> PResult<(Vec<u8>, Vec<u8>)> {
        if own_statement(self.node, ledger, st).as_ref() != Some(st) {
            return Err(PeerError::Refused("own view differs".into()));
        }
        let digest = st.digest(self.node.cfg.network());
        if let Some((k, d)) = self.node.store.decision(&st.log_key()).map_err(NodeError::from)? {
            if k != st.kind() || d != digest {
                return Err(PeerError::Refused("conflicting decision already signed".into()));
            }
        }
        let c = self.node.attestor.commit(session, &mut OsRng);
        let id = self.node.attestor.id.serialize();
        Ok((id, c.serialize().map_err(|e| PeerError::Refused(e.to_string()))?))
    }

    fn attest_sign(&mut self, ledger: &dyn Ledger, session: u64, st: &Statement, package: &[u8]) -> PResult<(Vec<u8>, Vec<u8>)> {
        let own = own_statement(self.node, ledger, st);
        let pkg = frost::SigningPackage::deserialize(package).map_err(|e| PeerError::Refused(e.to_string()))?;
        let share = self.node.attestor.sign(session, &pkg, st, own.as_ref(), &self.node.store)?;
        Ok((self.node.attestor.id.serialize(), share.serialize()))
    }

    fn spend_commit(&mut self, ledger: &dyn Ledger, session: u64, target: &SpendTarget, pczt: &[u8], now: i64) -> PResult<CommitResponse> {
        let kind = self.own_kind(ledger, target, now)?;
        self.zcashd()?
            .sign_commit(&CommitRequest { session, kind, pczt: pczt.to_vec() })
            .map_err(|e| PeerError::Refused(e.to_string()))
    }

    fn spend_share(&mut self, session: u64, commitments: &[CommitResponse]) -> PResult<ShareResponse> {
        self.zcashd()?
            .sign_share(&ShareRequest { session, commitments: commitments.to_vec() })
            .map_err(|e| PeerError::Refused(e.to_string()))
    }

    fn note_broadcast(&mut self, ledger: &dyn Ledger, notice: &BroadcastNotice, now: i64) -> PResult<()> {
        match &notice.target {
            SpendTarget::Payout { app } => {
                if let Ok(Ok(p)) = payout::plan(self.node, ledger, app, now) {
                    payout::record_broadcast(self.node, app, &p, &notice.txid, notice.expiry_height, now)?;
                }
            }
            SpendTarget::Refund { cmx, .. } => {
                let key = format!("refund_pending:{}", hex::encode(cmx));
                self.node.store.set_meta(&key, &notice.expiry_height.to_le_bytes()).map_err(NodeError::from)?;
            }
        }
        Ok(())
    }

    fn sync(&mut self, ledger: &dyn Ledger) -> PResult<()> {
        for app in self.node.apps.keys().copied().collect::<Vec<_>>() {
            self.node.sync_with_ledger(ledger, &app)?;
        }
        Ok(())
    }

    fn proposals(&mut self, ledger: &dyn Ledger) -> PResult<Proposals> {
        let mut out = Proposals::default();
        for app in self.node.apps.keys().copied().collect::<Vec<_>>() {
            self.node.sync_with_ledger(ledger, &app)?;
            match self.node.own_statements(ledger, &app) {
                Ok(v) => out.statements.extend(v.into_iter().map(|(_, s, _)| s)),
                Err(NodeError::Halted(_)) => return Ok(Proposals::default()),
                Err(e) => return Err(e.into()),
            }
            if let Ok(p) = self.node.own_paid_statements(ledger, &app) {
                out.paid.extend(p);
            }
        }
        Ok(out)
    }

    fn payout_proposal(&mut self, ledger: &dyn Ledger, app: &AppId, now: i64) -> PResult<Option<(payout::Planned, Proposal)>> {
        let planned = match payout::plan(self.node, ledger, app, now)? {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let proposal = self.zcashd()?.payout_propose(&planned.plan).map_err(|e| PeerError::Refused(e.to_string()))?;
        Ok(Some((planned, proposal)))
    }

    fn refund_proposals(&mut self, ledger: &dyn Ledger) -> PResult<Vec<(SpendTarget, RefundProposal)>> {
        if self.node.halted()?.is_some() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let tip = self.node.store.tip().map_err(NodeError::from)?.unwrap_or(0);
        for app in self.node.apps.keys().copied().collect::<Vec<_>>() {
            for (note, to) in self.node.refunds_due(ledger, &app)? {
                let key = format!("refund_pending:{}", hex::encode(note.note.cmx));
                if let Some(v) = self.node.store.meta(&key).map_err(NodeError::from)? {
                    if tip < u32::from_le_bytes(v.try_into().unwrap_or([0; 4])) {
                        continue;
                    }
                }
                match self.zcashd()?.refund_propose(&app, &note.note.cmx, &to) {
                    Ok(p) => out.push((SpendTarget::Refund { app, cmx: note.note.cmx, to: to.to_vec() }, p)),
                    Err(_) => continue,
                }
            }
        }
        Ok(out)
    }

    fn aggregate(&mut self, session: u64, pczt: &[u8], commitments: Vec<CommitResponse>, shares: Vec<ShareResponse>) -> PResult<Broadcast> {
        self.zcashd()?
            .sign_aggregate(&zol_types::api::AggregateRequest { session, pczt: pczt.to_vec(), commitments, shares })
            .map_err(|e| PeerError::Refused(e.to_string()))
    }

    fn app_ids(&self) -> Vec<AppId> {
        self.node.apps.keys().copied().collect()
    }

    fn program(&self, app: &AppId) -> Option<anchor_lang::prelude::Pubkey> {
        self.node.apps.get(app).map(|r| r.program)
    }

    fn wait_reason(&mut self, ledger: &dyn Ledger, app: &AppId, now: i64) -> Option<payout::Wait> {
        payout::plan(self.node, ledger, app, now).ok().and_then(|r| r.err())
    }
}

/// This host's operator, shared between the service loop and the HTTP server.
#[derive(Clone)]
pub struct SharedPeer(pub Arc<Mutex<crate::service::Operator>>);

macro_rules! with_local {
    ($self:ident, |$p:ident| $body:expr) => {{
        let mut g = $self.0.lock().map_err(|_| PeerError::Transport("poisoned".into()))?;
        let g = &mut *g;
        let mut $p = LocalPeer { node: &mut g.node, zcashd: Some(&g.zcashd) };
        $body
    }};
}

impl Peer for SharedPeer {
    fn name(&self) -> String {
        self.0.lock().map(|g| g.node.name.clone()).unwrap_or_default()
    }
    fn chain_report(&mut self) -> PResult<Option<(u32, [u8; 32])>> {
        with_local!(self, |p| p.chain_report())
    }
    fn attest_commit(&mut self, l: &dyn Ledger, s: u64, st: &Statement) -> PResult<(Vec<u8>, Vec<u8>)> {
        with_local!(self, |p| p.attest_commit(l, s, st))
    }
    fn attest_sign(&mut self, l: &dyn Ledger, s: u64, st: &Statement, pkg: &[u8]) -> PResult<(Vec<u8>, Vec<u8>)> {
        with_local!(self, |p| p.attest_sign(l, s, st, pkg))
    }
    fn spend_commit(&mut self, l: &dyn Ledger, s: u64, t: &SpendTarget, pczt: &[u8], now: i64) -> PResult<CommitResponse> {
        with_local!(self, |p| p.spend_commit(l, s, t, pczt, now))
    }
    fn spend_share(&mut self, s: u64, c: &[CommitResponse]) -> PResult<ShareResponse> {
        with_local!(self, |p| p.spend_share(s, c))
    }
    fn note_broadcast(&mut self, l: &dyn Ledger, n: &BroadcastNotice, now: i64) -> PResult<()> {
        with_local!(self, |p| p.note_broadcast(l, n, now))
    }
    fn sync(&mut self, l: &dyn Ledger) -> PResult<()> {
        with_local!(self, |p| p.sync(l))
    }
    fn proposals(&mut self, l: &dyn Ledger) -> PResult<Proposals> {
        with_local!(self, |p| p.proposals(l))
    }
    fn payout_proposal(&mut self, l: &dyn Ledger, a: &AppId, now: i64) -> PResult<Option<(payout::Planned, Proposal)>> {
        with_local!(self, |p| p.payout_proposal(l, a, now))
    }
    fn refund_proposals(&mut self, l: &dyn Ledger) -> PResult<Vec<(SpendTarget, RefundProposal)>> {
        with_local!(self, |p| p.refund_proposals(l))
    }
    fn aggregate(&mut self, s: u64, pczt: &[u8], c: Vec<CommitResponse>, sh: Vec<ShareResponse>) -> PResult<Broadcast> {
        with_local!(self, |p| p.aggregate(s, pczt, c, sh))
    }
    fn app_ids(&self) -> Vec<AppId> {
        self.0.lock().map(|g| g.node.apps.keys().copied().collect()).unwrap_or_default()
    }
    fn program(&self, app: &AppId) -> Option<anchor_lang::prelude::Pubkey> {
        self.0.lock().ok().and_then(|g| g.node.apps.get(app).map(|r| r.program))
    }
    fn wait_reason(&mut self, l: &dyn Ledger, a: &AppId, now: i64) -> Option<payout::Wait> {
        let mut g = self.0.lock().ok()?;
        let g = &mut *g;
        LocalPeer { node: &mut g.node, zcashd: Some(&g.zcashd) }.wait_reason(l, a, now)
    }
}

// ---------------------------------------------------------------- authenticated HTTP

/// Request authentication between operators: Ed25519 over
/// `"zol-peer-v2\n" ‖ path ‖ "\n" ‖ timestamp ‖ "\n" ‖ nonce ‖ "\n" ‖ hex(sha256(body))`,
/// with the sender's guardian key (registered on-chain in `Config.guardians`).
/// The nonce is 16 random bytes (hex) per request: Ed25519 is deterministic, so
/// without it two identical requests in one second would look like a replay.
/// Receivers accept only listed peers, timestamps within `MAX_SKEW_SECS`, and
/// each (peer, nonce) once. Run peer traffic over TLS or a private network.
pub const MAX_SKEW_SECS: i64 = 60;

pub fn signing_message(path: &str, ts: i64, nonce: &str, body: &[u8]) -> Vec<u8> {
    format!("zol-peer-v2\n{path}\n{ts}\n{nonce}\n{}", hex::encode(sha2::Sha256::digest(body))).into_bytes()
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The authentication headers of one request: peer key, timestamp, nonce, signature.
pub struct AuthHeaders<'a> {
    pub peer: &'a str,
    pub ts: &'a str,
    pub nonce: &'a str,
    pub sig: &'a str,
}

/// Verifies an incoming peer request. `seen` is the replay cache
/// ((peer key, nonce) → ts).
pub fn verify_request(
    peers: &[VerifyingKey],
    seen: &mut std::collections::HashMap<([u8; 32], [u8; 16]), i64>,
    path: &str,
    h: AuthHeaders,
    body: &[u8],
    now: i64,
) -> PResult<VerifyingKey> {
    let (who, ts, sig) = (h.peer, h.ts, h.sig);
    let bad = |m: &str| PeerError::Refused(format!("auth: {m}"));
    let who: [u8; 32] = hex::decode(who).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| bad("peer key"))?;
    let vk = VerifyingKey::from_bytes(&who).map_err(|_| bad("peer key"))?;
    if !peers.contains(&vk) {
        return Err(bad("unknown peer"));
    }
    let ts: i64 = ts.parse().map_err(|_| bad("timestamp"))?;
    if (now - ts).abs() > MAX_SKEW_SECS {
        return Err(bad("stale timestamp"));
    }
    let sig: [u8; 64] = hex::decode(sig).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| bad("signature"))?;
    let nonce: [u8; 16] = hex::decode(h.nonce).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| bad("nonce"))?;
    vk.verify_strict(&signing_message(path, ts, h.nonce, body), &ed25519_dalek::Signature::from_bytes(&sig))
        .map_err(|_| bad("signature"))?;
    seen.retain(|_, t| now - *t <= 2 * MAX_SKEW_SECS);
    if seen.insert((who, nonce), ts).is_some() {
        return Err(bad("replay"));
    }
    Ok(vk)
}

/// Another operator's gateway.
pub struct HttpPeer {
    pub name: String,
    base: String,
    key: SigningKey,
    http: reqwest::blocking::Client,
}

#[derive(Serialize, Deserialize)]
pub struct AttestReq {
    pub session: u64,
    pub statement: Statement,
    pub package: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
pub struct SpendCommitReq {
    pub session: u64,
    pub target: SpendTarget,
    #[serde(with = "zol_types::b64")]
    pub pczt: Vec<u8>,
}

/// A deposit intent a page registered with one operator, passed on to the
/// others. Each receiver verifies the session signature itself.
#[derive(Serialize, Deserialize)]
pub struct IntentNotice {
    pub app: AppId,
    #[serde(with = "zol_types::b64")]
    pub memo: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct SpendShareReq {
    pub session: u64,
    pub commitments: Vec<CommitResponse>,
}

impl HttpPeer {
    /// `key` is this host's guardian seed, used to sign requests.
    pub fn new(name: &str, base: &str, key_seed: &[u8; 32]) -> Self {
        Self {
            name: name.to_string(),
            base: base.trim_end_matches('/').to_string(),
            key: SigningKey::from_bytes(key_seed),
            http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(300)).build().expect("client"),
        }
    }

    fn call<B: Serialize, T: serde::de::DeserializeOwned>(&self, path: &str, body: &B) -> PResult<T> {
        let body = serde_json::to_vec(body).map_err(|e| PeerError::Transport(e.to_string()))?;
        let ts = now_secs();
        let nonce = hex::encode(rand::random::<[u8; 16]>());
        let sig = self.key.sign(&signing_message(path, ts, &nonce, &body));
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .header("x-zol-peer", hex::encode(self.key.verifying_key().to_bytes()))
            .header("x-zol-ts", ts.to_string())
            .header("x-zol-nonce", nonce)
            .header("x-zol-sig", hex::encode(sig.to_bytes()))
            .body(body)
            .send()
            .map_err(|e| PeerError::Transport(e.to_string()))?;
        let ok = r.status().is_success();
        let text = r.text().map_err(|e| PeerError::Transport(e.to_string()))?;
        if ok {
            serde_json::from_str(&text).map_err(|e| PeerError::Transport(format!("{e}: {text}")))
        } else {
            Err(serde_json::from_str::<PeerError>(&text).unwrap_or(PeerError::Transport(text)))
        }
    }
}

impl HttpPeer {
    /// Passes a page's intent on. Not part of `Peer`: it is gossip, not a round.
    pub fn send_intent(&self, app: &AppId, memo: &[u8]) -> PResult<()> {
        self.call("/peer/intent", &IntentNotice { app: *app, memo: memo.to_vec() })
    }
}

impl Peer for HttpPeer {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn chain_report(&mut self) -> PResult<Option<(u32, [u8; 32])>> {
        self.call("/peer/chain_report", &())
    }
    fn attest_commit(&mut self, _l: &dyn Ledger, session: u64, st: &Statement) -> PResult<(Vec<u8>, Vec<u8>)> {
        self.call("/peer/attest_commit", &AttestReq { session, statement: st.clone(), package: None })
    }
    fn attest_sign(&mut self, _l: &dyn Ledger, session: u64, st: &Statement, package: &[u8]) -> PResult<(Vec<u8>, Vec<u8>)> {
        self.call("/peer/attest_sign", &AttestReq { session, statement: st.clone(), package: Some(package.to_vec()) })
    }
    fn spend_commit(&mut self, _l: &dyn Ledger, session: u64, target: &SpendTarget, pczt: &[u8], _now: i64) -> PResult<CommitResponse> {
        self.call("/peer/spend_commit", &SpendCommitReq { session, target: target.clone(), pczt: pczt.to_vec() })
    }
    fn spend_share(&mut self, session: u64, commitments: &[CommitResponse]) -> PResult<ShareResponse> {
        self.call("/peer/spend_share", &SpendShareReq { session, commitments: commitments.to_vec() })
    }
    fn note_broadcast(&mut self, _l: &dyn Ledger, notice: &BroadcastNotice, _now: i64) -> PResult<()> {
        self.call("/peer/note_broadcast", notice)
    }
    fn sync(&mut self, _l: &dyn Ledger) -> PResult<()> {
        Ok(())
    }
    fn proposals(&mut self, _l: &dyn Ledger) -> PResult<Proposals> {
        Err(PeerError::NotLocal("proposals".into()))
    }
    fn payout_proposal(&mut self, _l: &dyn Ledger, _a: &AppId, _now: i64) -> PResult<Option<(payout::Planned, Proposal)>> {
        Err(PeerError::NotLocal("payout_proposal".into()))
    }
    fn refund_proposals(&mut self, _l: &dyn Ledger) -> PResult<Vec<(SpendTarget, RefundProposal)>> {
        Err(PeerError::NotLocal("refund_proposals".into()))
    }
    fn aggregate(&mut self, _s: u64, _p: &[u8], _c: Vec<CommitResponse>, _sh: Vec<ShareResponse>) -> PResult<Broadcast> {
        Err(PeerError::NotLocal("aggregate".into()))
    }
    fn app_ids(&self) -> Vec<AppId> {
        vec![]
    }
    fn program(&self, _app: &AppId) -> Option<anchor_lang::prelude::Pubkey> {
        None
    }
    fn wait_reason(&mut self, _l: &dyn Ledger, _a: &AppId, _now: i64) -> Option<payout::Wait> {
        None
    }
}
