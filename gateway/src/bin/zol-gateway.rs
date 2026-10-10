//! `zol-gateway`: the Solana half of the gateway operators, in cluster mode
//! (all operators' nodes in one process; see `service`).
//!
//!   zol-gateway keygen --operators 3 --threshold 2 --out keys/
//!   zol-gateway run --config gateway.json

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anchor_lang::prelude::Pubkey;
use clap::{Parser, Subcommand};
use frost_ed25519::keys::{KeyPackage, PublicKeyPackage};
use rand::rngs::OsRng;
use serde::Deserialize;
use solana_keypair::Keypair;
use zol_gateway::attestor::{dkg, group_key, Attestor};
use zol_gateway::config::{AppSecrets, GatewayConfig};
use zol_gateway::node::{AppRuntime, Node};
use zol_gateway::rpc::RpcLedger;
use zol_gateway::service::{Cluster, Operator};
use zol_gateway::store::Store;
use zol_gateway::zcashd::{OrchardReceivers, ZcashdClient};

#[derive(Parser)]
#[command(about = "ZOL gateway (Solana side)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Distributed key generation for the Ed25519 attestor key.
    Keygen {
        #[arg(long, default_value_t = 3)]
        operators: u16,
        #[arg(long, default_value_t = 2)]
        threshold: u16,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run the service loop.
    Run {
        #[arg(long)]
        config: PathBuf,
        /// Stop after this many rounds (0 = forever).
        #[arg(long, default_value_t = 0)]
        rounds: u64,
    },
}

#[derive(Deserialize)]
struct OperatorCfg {
    name: String,
    store: PathBuf,
    zcashd: String,
    /// Solana keypair JSON (64-byte array) of this operator's guardian key.
    guardian_keypair: PathBuf,
    /// FROST key package JSON written by `keygen`.
    attestor_share: PathBuf,
}

#[derive(Deserialize)]
struct AppCfg {
    /// App program ID, base58.
    program: String,
    /// Hex, 32 bytes: master secret for this deployment's gateway keys.
    gateway_master_secret: String,
}

#[derive(Deserialize)]
struct Config {
    #[serde(flatten)]
    gateway: GatewayConfig,
    solana_rpc: String,
    relayer_keypair: PathBuf,
    attestor_pubkeys: PathBuf,
    threshold: usize,
    #[serde(default)]
    coordinator: usize,
    operators: Vec<OperatorCfg>,
    apps: Vec<AppCfg>,
    #[serde(default = "default_poll")]
    poll_ms: u64,
}

fn default_poll() -> u64 {
    1_000
}

type BoxErr = Box<dyn std::error::Error>;

fn read_keypair(p: &Path) -> Result<Keypair, BoxErr> {
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(p)?)?;
    let seed: [u8; 32] = bytes[..32].try_into()?;
    Ok(Keypair::new_from_array(seed))
}

fn hex32(s: &str) -> Result<[u8; 32], BoxErr> {
    Ok(hex::decode(s)?.try_into().map_err(|_| "expected 32 bytes")?)
}

fn keygen(n: u16, t: u16, out: &Path) -> Result<(), BoxErr> {
    std::fs::create_dir_all(out)?;
    let (keys, pubkeys) = dkg(n, t, &mut OsRng)?;
    for (i, k) in keys.iter().enumerate() {
        std::fs::write(out.join(format!("attestor-share-{i}.json")), serde_json::to_string_pretty(k)?)?;
    }
    std::fs::write(out.join("attestor-pubkeys.json"), serde_json::to_string_pretty(&pubkeys)?)?;
    println!("attestor group key (set as Config.attestor): {}", Pubkey::new_from_array(group_key(&pubkeys)));
    Ok(())
}

fn run(path: &Path, rounds: u64) -> Result<(), BoxErr> {
    let cfg: Config = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let pubkeys: PublicKeyPackage = serde_json::from_str(&std::fs::read_to_string(&cfg.attestor_pubkeys)?)?;
    let network = cfg.gateway.network();
    let mut ledger = RpcLedger::new(&cfg.solana_rpc, read_keypair(&cfg.relayer_keypair)?, group_key(&pubkeys), cfg.gateway.network);
    ledger.send_commitment = "confirmed";

    let mut operators = Vec::new();
    for o in &cfg.operators {
        let share: KeyPackage = serde_json::from_str(&std::fs::read_to_string(&o.attestor_share)?)?;
        let zcashd = ZcashdClient::new(&o.zcashd);
        let mut apps = BTreeMap::new();
        for a in &cfg.apps {
            let program: Pubkey = a.program.parse()?;
            let app_id = program.to_bytes();
            let vault = zcashd.vault(&app_id)?;
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
            name: o.name.clone(),
            store: Store::open(o.store.to_str().ok_or("store path")?)?,
            cfg: cfg.gateway.clone(),
            attestor: Attestor::new(share, pubkeys.clone(), network),
            guardian_seed: *read_keypair(&o.guardian_keypair)?.secret_bytes(),
            apps,
            addresses: Box::new(OrchardReceivers),
        };
        operators.push(Operator::new(node, zcashd)?);
    }
    let mut cluster = Cluster { operators, pubkeys, threshold: cfg.threshold, coordinator: cfg.coordinator, session: 0 };
    let mut n = 0u64;
    loop {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        match cluster.tick(&mut ledger, now, &mut OsRng) {
            Ok(r) => {
                if r.events > 0 || !r.round.credited.is_empty() || !r.round.voided.is_empty() || !r.guard.is_empty() || !r.refunds.is_empty() {
                    println!(
                        "events={} credited={} voided={} failed={} guard={:?} refunds={} halted={:?}",
                        r.events,
                        r.round.credited.len(),
                        r.round.voided.len(),
                        r.round.failed.len(),
                        r.guard,
                        r.refunds.len(),
                        r.halted
                    );
                }
            }
            Err(e) => eprintln!("tick: {e}"),
        }
        n += 1;
        if rounds != 0 && n >= rounds {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(cfg.poll_ms));
    }
}

fn main() -> Result<(), BoxErr> {
    match Cli::parse().cmd {
        Cmd::Keygen { operators, threshold, out } => keygen(operators, threshold, &out),
        Cmd::Run { config, rounds } => run(&config, rounds),
    }
}
