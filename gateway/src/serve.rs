//! One operator per host (`zol-gateway serve`).
//!
//! The loop keeps this operator's node current (its own zcashd events, head
//! chains, reconciliation and guardian duties, chain agreement with peers).
//! The coordinator role rotates by time epoch over `order`; in its epoch this
//! host runs the rounds over itself and its peers. Two hosts that briefly both
//! act are safe: the ledger admits one `Deposit` account per ID, and two vault
//! transitions share the head's nullifier.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anchor_lang::prelude::Pubkey;
use ed25519_dalek::VerifyingKey;
use frost_ed25519::keys::{KeyPackage, PublicKeyPackage};
use rand::rngs::OsRng;
use serde::Deserialize;
use solana_keypair::Keypair;

use crate::attestor::{group_key, Attestor};
use crate::config::{AppSecrets, GatewayConfig};
use crate::node::{AppRuntime, Node};
use crate::peer::{now_secs, HttpPeer, Peer, SharedPeer};
use crate::rpc::RpcLedger;
use crate::rounds;
use crate::server::{self, ServerState};
use crate::service::Operator;
use crate::store::Store;
use crate::zcashd::{ZcashdClient, ZcashdReceivers};

#[derive(Clone, Debug, Deserialize)]
pub struct PeerCfg {
    pub name: String,
    pub url: String,
    /// Base58 guardian public key (as in `Config.guardians`).
    pub guardian: String,
    /// Its page API as pages reach it, if different from `url` (which may be
    /// a private address).
    #[serde(default)]
    pub public_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AppCfg {
    pub program: String,
    pub gateway_master_secret: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServeConfig {
    #[serde(flatten)]
    pub gateway: GatewayConfig,
    pub name: String,
    pub listen: String,
    /// This host's page API as pages reach it, listed in `AppInfo.operators`.
    #[serde(default)]
    pub public_url: Option<String>,
    pub solana_rpc: String,
    /// This operator's relayer (fee payer) keypair JSON.
    pub relayer_keypair: PathBuf,
    pub store: PathBuf,
    pub zcashd: String,
    pub guardian_keypair: PathBuf,
    pub attestor_share: PathBuf,
    pub attestor_pubkeys: PathBuf,
    pub threshold: usize,
    pub apps: Vec<AppCfg>,
    /// The other operators.
    pub peers: Vec<PeerCfg>,
    /// Coordinator rotation order: every operator's name, including this one.
    pub order: Vec<String>,
    #[serde(default = "default_epoch")]
    pub epoch_secs: i64,
    #[serde(default = "default_poll")]
    pub poll_ms: u64,
    /// Commitment a relayed transaction must reach ("confirmed" or "finalized").
    #[serde(default = "default_commitment")]
    pub send_commitment: String,
}

fn default_epoch() -> i64 {
    30
}
fn default_poll() -> u64 {
    1_000
}
fn default_commitment() -> String {
    "confirmed".into()
}

pub type BoxErr = Box<dyn std::error::Error + Send + Sync>;

pub fn read_keypair(p: &std::path::Path) -> Result<Keypair, BoxErr> {
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(p)?)?;
    let seed: [u8; 32] = bytes[..32].try_into()?;
    Ok(Keypair::new_from_array(seed))
}

fn hex32(s: &str) -> Result<[u8; 32], BoxErr> {
    Ok(hex::decode(s)?.try_into().map_err(|_| "expected 32 bytes")?)
}

pub struct Handle {
    pub stop: Arc<AtomicBool>,
    pub thread: std::thread::JoinHandle<()>,
    pub server: Arc<tiny_http::Server>,
    pub local: SharedPeer,
}

impl Handle {
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.server.unblock();
        let _ = self.thread.join();
    }
}

fn ledger(cfg: &ServeConfig, attestor: [u8; 32]) -> Result<RpcLedger, BoxErr> {
    let mut l = RpcLedger::new(&cfg.solana_rpc, read_keypair(&cfg.relayer_keypair)?, attestor, cfg.gateway.network);
    l.send_commitment = if cfg.send_commitment == "finalized" { "finalized" } else { "confirmed" };
    Ok(l)
}

/// Builds the operator, starts its HTTP server and its loop.
pub fn start(cfg: ServeConfig) -> Result<Handle, BoxErr> {
    let pubkeys: PublicKeyPackage = serde_json::from_str(&std::fs::read_to_string(&cfg.attestor_pubkeys)?)?;
    let share: KeyPackage = serde_json::from_str(&std::fs::read_to_string(&cfg.attestor_share)?)?;
    let network = cfg.gateway.network();
    let guardian = read_keypair(&cfg.guardian_keypair)?;
    let zcashd = ZcashdClient::new(&cfg.zcashd);
    let mut apps = BTreeMap::new();
    for a in &cfg.apps {
        let program: Pubkey = a.program.parse()?;
        let app_id = program.to_bytes();
        let vault = zcashd.vault(&app_id).map_err(|e| e.to_string())?;
        apps.insert(
            app_id,
            AppRuntime {
                program,
                vault_address: vault.address.as_slice().try_into().map_err(|_| "vault address length")?,
                secrets: AppSecrets::derive(&hex32(&a.gateway_master_secret)?, network, &app_id),
                session_index: Box::new(move |s: &[u8; 32]| zol_types::session_receiver_index(&app_id, s)),
            },
        );
    }
    let node = Node {
        heads: Default::default(),
        name: cfg.name.clone(),
        store: Store::open(cfg.store.to_str().ok_or("store path")?)?,
        cfg: cfg.gateway.clone(),
        attestor: Attestor::new(share, pubkeys.clone(), network),
        guardian_seed: *guardian.secret_bytes(),
        apps,
        addresses: Box::new(ZcashdReceivers::new(&cfg.zcashd)),
    };
    let op = Operator::new(node, zcashd).map_err(|e| e.to_string())?;
    let local = SharedPeer(Arc::new(Mutex::new(op)));

    let mut allowed = Vec::new();
    for p in &cfg.peers {
        let pk: Pubkey = p.guardian.parse()?;
        allowed.push(VerifyingKey::from_bytes(&pk.to_bytes())?);
    }
    let attestor = group_key(&pubkeys);
    let state = Arc::new(ServerState {
        local: local.clone(),
        ledger: Mutex::new(Box::new(ledger(&cfg, attestor)?)),
        peers: allowed,
        seen: Mutex::new(HashMap::new()),
        gossip: cfg.peers.iter().map(|p| (p.name.clone(), p.url.clone())).collect(),
        gossip_seed: *guardian.secret_bytes(),
        operators: cfg
            .order
            .iter()
            .filter_map(|n| {
                if *n == cfg.name {
                    cfg.public_url.clone()
                } else {
                    cfg.peers.iter().find(|p| p.name == *n).map(|p| p.public_url.clone().unwrap_or_else(|| p.url.clone()))
                }
            })
            .collect(),
    });
    let server = server::spawn(&cfg.listen, state)?;

    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        let local = local.clone();
        let mut ledger = ledger(&cfg, attestor)?;
        let seed = *guardian.secret_bytes();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if let Err(e) = tick(&cfg, &local, &mut ledger, &pubkeys, &seed) {
                    eprintln!("[{}] tick: {e}", cfg.name);
                }
                std::thread::sleep(Duration::from_millis(cfg.poll_ms));
            }
        })
    };
    Ok(Handle { stop, thread, server, local })
}

/// Whose epoch it is.
pub fn coordinator_at(order: &[String], epoch_secs: i64, now: i64) -> usize {
    ((now / epoch_secs.max(1)) as usize) % order.len().max(1)
}

fn tick(cfg: &ServeConfig, local: &SharedPeer, ledger: &mut RpcLedger, pubkeys: &PublicKeyPackage, seed: &[u8; 32]) -> Result<(), BoxErr> {
    let now = now_secs();
    // Keep this operator current.
    {
        let mut g = local.0.lock().map_err(|_| "poisoned")?;
        g.pull(now).map_err(|e| e.to_string())?;
        g.node.check_stall(now).map_err(|e| e.to_string())?;
        g.refresh_heads(ledger).map_err(|e| e.to_string())?;
        for a in g.node.guard(ledger).map_err(|e| e.to_string())? {
            eprintln!("[{}] guard: {a:?}", cfg.name);
        }
    }
    // Peers in rotation order; this host is a SharedPeer, the rest are remote.
    let mut peers: Vec<Box<dyn Peer>> = Vec::new();
    let mut me = 0;
    for (i, name) in cfg.order.iter().enumerate() {
        if *name == cfg.name {
            me = i;
            peers.push(Box::new(local.clone()));
        } else if let Some(p) = cfg.peers.iter().find(|p| p.name == *name) {
            peers.push(Box::new(HttpPeer::new(&p.name, &p.url, seed)));
        }
    }
    // Chain agreement with every reachable peer.
    let reports = rounds::chain_reports(&mut peers);
    local.0.lock().map_err(|_| "poisoned")?.node.check_agreement(&reports).map_err(|e| e.to_string())?;

    // Act only in the first half of this host's epoch, so a round that is still
    // running (proving takes seconds) ends before the next coordinator starts.
    if coordinator_at(&cfg.order, cfg.epoch_secs, now) != me || now.rem_euclid(cfg.epoch_secs.max(1)) * 2 >= cfg.epoch_secs {
        return Ok(());
    }
    let net = cfg.gateway.network();
    let round = rounds::attest_rounds(&mut peers, me, ledger, net, pubkeys, cfg.threshold, &mut OsRng);
    if !round.credited.is_empty() || !round.voided.is_empty() || !round.marked_paid.is_empty() {
        eprintln!(
            "[{}] coordinator: credited={} voided={} marked_paid={:?}",
            cfg.name,
            round.credited.len(),
            round.voided.len(),
            round.marked_paid
        );
    }
    let spends = rounds::spend_rounds(&mut peers, me, ledger, cfg.threshold, now, &mut OsRng);
    if !spends.refunds.is_empty() || !spends.payouts.is_empty() || !spends.failed.is_empty() {
        eprintln!(
            "[{}] coordinator: refunds={} payouts={} failed={:?}",
            cfg.name,
            spends.refunds.len(),
            spends.payouts.len(),
            spends.failed
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg: ServeConfig = serde_json::from_str(include_str!("../examples/serve.example.json")).unwrap();
        assert_eq!(cfg.order.len(), cfg.peers.len() + 1);
        assert!(cfg.public_url.is_some() && cfg.peers.iter().all(|p| p.public_url.is_some()));
        assert_eq!(cfg.gateway.rule().nu7_height, crate::config::TESTNET_NU7_HEIGHT);
    }
}
