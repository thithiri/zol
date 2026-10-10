//! `zol-gateway`: the Solana half of the gateway operators, in cluster mode
//! (all operators' nodes in one process; see `service`).
//!
//!   zol-gateway keygen --operators 3 --threshold 2 --out keys/
//!   zol-gateway dkg --config serve.json --ceremony <id>   (on every host at once)
//!   zol-gateway serve --config serve.json
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
use solana_signer::Signer;
use zol_gateway::attestor::{dkg, group_key, Attestor};
use zol_gateway::config::{AppSecrets, GatewayConfig};
use zol_gateway::node::{AppRuntime, Node};
use zol_gateway::rpc::RpcLedger;
use zol_gateway::service::{Cluster, Operator};
use zol_gateway::store::Store;
use zol_gateway::zcashd::{ZcashdClient, ZcashdReceivers};

#[derive(Parser)]
#[command(about = "ZOL gateway (Solana side)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Attestor key generation in one process (local devnet, tests).
    Keygen {
        #[arg(long, default_value_t = 3)]
        operators: u16,
        #[arg(long, default_value_t = 2)]
        threshold: u16,
        #[arg(long)]
        out: PathBuf,
    },
    /// Attestor DKG across hosts: run on every operator's host at about the
    /// same time, with the same `--ceremony`. Writes `attestor_share` and
    /// `attestor_pubkeys` from the serve config.
    Dkg {
        #[arg(long)]
        config: PathBuf,
        /// Label shared by all operators for this ceremony, e.g. "testnet-2026-10".
        #[arg(long)]
        ceremony: String,
        #[arg(long, default_value_t = 600)]
        timeout_secs: u64,
    },
    /// Run one operator on this host (multi-host deployment).
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Run every operator in this process (cluster mode: local devnet, tests).
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

fn dkg_over_peers(path: &Path, ceremony: String, timeout_secs: u64) -> Result<(), BoxErr> {
    use zol_gateway::dkgnet::{self, DkgParams, Participant};
    let cfg: zol_gateway::serve::ServeConfig = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    for p in [&cfg.attestor_share, &cfg.attestor_pubkeys] {
        if p.exists() {
            return Err(format!("{} exists; move it away to run a new ceremony", p.display()).into());
        }
    }
    let guardian = read_keypair(&cfg.guardian_keypair)?;
    let mut participants = Vec::new();
    for name in &cfg.order {
        let (url, key) = if *name == cfg.name {
            (String::new(), guardian.pubkey().to_bytes())
        } else {
            let p = cfg.peers.iter().find(|p| p.name == *name).ok_or_else(|| format!("{name} is in order but not in peers"))?;
            (p.url.clone(), p.guardian.parse::<Pubkey>()?.to_bytes())
        };
        participants.push(Participant { name: name.clone(), url, guardian: ed25519_dalek::VerifyingKey::from_bytes(&key)? });
    }
    let params = DkgParams {
        ceremony,
        threshold: cfg.threshold as u16,
        participants,
        me: cfg.name.clone(),
        guardian_seed: *guardian.secret_bytes(),
        listen: cfg.listen.clone(),
        timeout: Duration::from_secs(timeout_secs),
    };
    println!("{}: waiting for {} peers on {}", cfg.name, cfg.peers.len(), cfg.listen);
    let (share, pubkeys) = dkgnet::run(&params, &mut OsRng)?;
    for p in [&cfg.attestor_share, &cfg.attestor_pubkeys] {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
    }
    std::fs::write(&cfg.attestor_share, serde_json::to_string_pretty(&share)?)?;
    std::fs::write(&cfg.attestor_pubkeys, serde_json::to_string_pretty(&pubkeys)?)?;
    println!("attestor group key (set as Config.attestor): {}", Pubkey::new_from_array(group_key(&pubkeys)));
    println!("every operator must print the same key");
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
            heads: Default::default(),
            name: o.name.clone(),
            store: Store::open(o.store.to_str().ok_or("store path")?)?,
            cfg: cfg.gateway.clone(),
            attestor: Attestor::new(share, pubkeys.clone(), network),
            guardian_seed: *read_keypair(&o.guardian_keypair)?.secret_bytes(),
            apps,
            addresses: Box::new(ZcashdReceivers::new(&o.zcashd)),
        };
        operators.push(Operator::new(node, zcashd)?);
    }
    let mut cluster = Cluster { operators, pubkeys, threshold: cfg.threshold, coordinator: cfg.coordinator, session: 0, attempts: Default::default() };
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
        Cmd::Dkg { config, ceremony, timeout_secs } => dkg_over_peers(&config, ceremony, timeout_secs),
        Cmd::Run { config, rounds } => run(&config, rounds),
        Cmd::Serve { config } => {
            let cfg: zol_gateway::serve::ServeConfig = serde_json::from_str(&std::fs::read_to_string(&config)?)?;
            let name = cfg.name.clone();
            let listen = cfg.listen.clone();
            let h = zol_gateway::serve::start(cfg).map_err(|e| e.to_string())?;
            println!("{name} serving on {listen}");
            h.thread.join().map_err(|_| "loop panicked")?;
            Ok(())
        }
    }
}
