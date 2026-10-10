//! Test fixtures: a `Ledger` backed by the real ledger program on LiteSVM, a
//! three-operator gateway cluster with a DKG attestor key, and a fake chain.
#![allow(dead_code)]

pub mod regtest;

use anchor_lang::prelude::Pubkey;
use ed25519_dalek::{Signer as _, SigningKey};
use frost_ed25519::keys::PublicKeyPackage;
use rand::rngs::OsRng;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use zol_gateway::attestor::{dkg, Attestor};
use zol_gateway::chain::{AppId, ChainEvent, PermissiveAddresses, ScannedNote, Scope};
use zol_gateway::config::{AppSecrets, Depths, GatewayConfig};
use zol_gateway::ledger::{Ledger, LedgerError, Result};
use zol_gateway::node::{coordinate, AppRuntime, Node, Round};
use zol_gateway::store::Store;
use zol_ledger::state::{App, Balance, Config, Deposit, Settlement};
use zol_ledger::{CreditArgs, SetConfigArgs};
use zol_tests::*;
use zol_wire::{DepositMemo, HeadMemo, Network};

pub const VAULT: [u8; 43] = [0x33; 43];
pub const DENOM: u64 = 1_000_000;
pub const MASTER: [u8; 32] = [0x5E; 32];

pub fn app_id() -> AppId {
    TEST_APP.to_bytes()
}

pub struct SvmLedger {
    pub env: Env,
    pub group_key: [u8; 32],
}

fn tx_err(r: TxResult) -> Result<()> {
    r.map(|_| ()).map_err(|f| LedgerError::Tx(format!("{:?} {}", f.err, f.meta.logs.join(" | "))))
}

impl SvmLedger {
    fn precompile(&self, digest: &[u8; 32], sig: &[u8; 64]) -> Instruction {
        let ix = solana_ed25519_program::new_ed25519_instruction_with_signature(digest, sig, &self.group_key);
        Instruction { program_id: ix.program_id, accounts: vec![], data: ix.data }
    }

    fn accounts<T: anchor_lang::AccountDeserialize>(&self) -> Vec<T> {
        self.env
            .svm
            .get_program_accounts(&LEDGER)
            .into_iter()
            .filter_map(|(_, a)| T::try_deserialize(&mut &a.data[..]).ok())
            .collect()
    }
}

impl Ledger for SvmLedger {
    fn config(&self) -> Result<Config> {
        Ok(self.env.account(&config_pda()))
    }
    fn app(&self, p: &Pubkey) -> Result<Option<App>> {
        Ok(self.env.try_account(&app_pda(p)))
    }
    fn deposit(&self, id: &[u8; 32]) -> Result<Option<Deposit>> {
        Ok(self.env.try_account(&deposit_pda(id)))
    }
    fn deposits(&self, p: &Pubkey) -> Result<Vec<([u8; 32], Deposit)>> {
        let app = app_pda(p);
        Ok(self.accounts::<Deposit>().into_iter().filter(|d| d.app == app).map(|d| (d.id, d)).collect())
    }
    fn balance(&self, p: &Pubkey, s: &Pubkey) -> Result<Option<Balance>> {
        Ok(self.env.try_account(&balance_pda(&app_pda(p), s)))
    }
    fn settlements(&self, p: &Pubkey) -> Result<Vec<Settlement>> {
        let app = app_pda(p);
        Ok(self.accounts::<Settlement>().into_iter().filter(|s| s.app == app).collect())
    }
    fn now(&self) -> Result<i64> {
        Ok(self.env.now)
    }
    fn submit_credit(&mut self, _p: &Pubkey, args: CreditArgs, sig: [u8; 64]) -> Result<()> {
        let digest = Env::credit_digest_on(self.env.network, &args, &TEST_APP);
        let pre = self.precompile(&digest, &sig);
        let ix = self.env.credit_ix(args);
        let r = self.env.relayer.insecure_clone();
        tx_err(self.env.send(&[pre, ix], &[&r]))
    }
    fn submit_void(&mut self, _p: &Pubkey, id: [u8; 32], sig: [u8; 64]) -> Result<()> {
        let digest = zol_wire::digest::void_digest::<zol_wire::digest::Sha256>(self.env.network, &TEST_APP.to_bytes(), &id);
        let pre = self.precompile(&digest, &sig);
        let ix = ledger_ix(
            zol_ledger::accounts::VoidDeposit {
                relayer: self.env.relayer.pubkey(),
                config: config_pda(),
                app: app_pda(&TEST_APP),
                deposit: deposit_pda(&id),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::VoidDeposit { deposit_id: id },
        );
        let r = self.env.relayer.insecure_clone();
        tx_err(self.env.send(&[pre, ix], &[&r]))
    }
    fn submit_paid(&mut self, _p: &Pubkey, seq: u64, sig: [u8; 64]) -> Result<()> {
        let digest = zol_wire::digest::paid_digest::<zol_wire::digest::Sha256>(self.env.network, &TEST_APP.to_bytes(), seq);
        let pre = self.precompile(&digest, &sig);
        let app = app_pda(&TEST_APP);
        let ix = ledger_ix(
            zol_ledger::accounts::MarkPaid {
                relayer: self.env.relayer.pubkey(),
                config: config_pda(),
                app,
                settlement: settlement_pda(&app, seq),
                instructions: IX_SYSVAR,
            },
            zol_ledger::instruction::MarkPaid {},
        );
        let r = self.env.relayer.insecure_clone();
        tx_err(self.env.send(&[pre, ix], &[&r]))
    }
    fn freeze(&mut self, seed: &[u8; 32], p: &Pubkey) -> Result<()> {
        let g = Keypair::new_from_array(*seed);
        let ix = ledger_ix(
            zol_ledger::accounts::FreezeApp { guardian: g.pubkey(), config: config_pda(), app: app_pda(p) },
            zol_ledger::instruction::FreezeApp {},
        );
        tx_err(self.env.send(&[ix], &[&g]))
    }
}

pub fn gw_config() -> GatewayConfig {
    GatewayConfig {
        network: Network::Testnet as u8,
        depths: Depths { early: 10, full: 100, payout: 100 },
        early_credit_cap: 5_000_000,
        tip_stall_secs: 1_800,
        payout: Default::default(),
        // The scripted chain starts low: count its blocks once.
        nu7_height: Some(0),
    }
}

pub fn session_index(app: AppId) -> impl Fn(&[u8; 32]) -> u128 {
    move |sk: &[u8; 32]| zol_types::session_receiver_index(&app, sk)
}

/// Three operator nodes for the test app, one per key share and guardian seed.
pub fn make_nodes(keys: Vec<frost_ed25519::keys::KeyPackage>, pubkeys: &PublicKeyPackage, guardian_seeds: &[[u8; 32]]) -> Vec<Node> {
    keys.into_iter()
        .enumerate()
        .map(|(i, k)| {
            let mut apps = std::collections::BTreeMap::new();
            apps.insert(
                app_id(),
                AppRuntime {
                    program: TEST_APP,
                    vault_address: VAULT,
                    secrets: AppSecrets::derive(&MASTER, Network::Testnet, &app_id()),
                    session_index: Box::new(session_index(app_id())),
                },
            );
            Node {
                heads: Default::default(),
                suspect: Default::default(),
                name: format!("op{i}"),
                store: Store::in_memory().unwrap(),
                cfg: gw_config(),
                attestor: Attestor::new(k, pubkeys.clone(), Network::Testnet),
                guardian_seed: guardian_seeds[i],
                apps,
                addresses: Box::new(PermissiveAddresses),
            }
        })
        .collect()
}

pub struct World {
    pub ledger: SvmLedger,
    pub nodes: Vec<Node>,
    pub pubkeys: PublicKeyPackage,
    pub session: u64,
    pub height: u32,
    pub next_cmx: u8,
}

impl World {
    /// Ledger with an active test app; three operators sharing a DKG attestor
    /// key (2-of-3) that the ledger's config now names.
    pub fn new() -> Self {
        Self::with_env(Env::new())
    }

    pub fn with_env(mut env: Env) -> Self {
        let (keys, pubkeys) = dkg(3, 2, &mut OsRng).unwrap();
        let group_key = zol_gateway::attestor::group_key(&pubkeys);
        if env.try_account::<Config>(&config_pda()).is_some() {
            ok!(env.set_config(SetConfigArgs {
                new_admin: None,
                attestor: Some(Pubkey::new_from_array(group_key)),
                params: None,
                paused: None,
            }));
        }
        let seeds: Vec<[u8; 32]> = env.guardians.iter().map(|g| *g.secret_bytes()).collect();
        let nodes = make_nodes(keys, &pubkeys, &seeds);
        World { ledger: SvmLedger { env, group_key }, nodes, pubkeys, session: 0, height: 0, next_cmx: 1 }
    }

    /// Feeds `ev` to the given nodes (all if `only` is None).
    pub fn feed(&mut self, ev: ChainEvent, only: Option<&[usize]>) {
        let now = self.ledger.env.now;
        for (i, n) in self.nodes.iter().enumerate() {
            if only.is_none_or(|o| o.contains(&i)) {
                n.apply(&ev, now).unwrap();
            }
        }
    }

    pub fn mine(&mut self, blocks: u32) {
        for _ in 0..blocks {
            self.height += 1;
            let h = self.height;
            self.feed(ChainEvent::Block { height: h, hash: block_hash(h, 0) }, None);
        }
    }

    pub fn note(&mut self, memo: &[u8; 512], value: u64, scope: Scope, receiver_index: u128) -> ScannedNote {
        let cmx = [self.next_cmx; 32];
        self.next_cmx += 1;
        ScannedNote {
            app: app_id(),
            cmx,
            nullifier: [cmx[0] ^ 0xAA; 32],
            value,
            memo: memo.to_vec(),
            receiver: VAULT.to_vec(),
            receiver_index,
            scope,
            height: self.height,
            block_hash: block_hash(self.height, 0),
            txid: cmx,
        }
    }

    /// A genesis head carrying `float` in the vault's internal scope.
    pub fn genesis(&mut self, float: u64) -> ScannedNote {
        let memo = HeadMemo::genesis(Network::Testnet, app_id()).encode();
        let n = self.note(&memo, float, Scope::Internal, 0);
        self.feed(ChainEvent::Note(n.clone()), None);
        n
    }

    pub fn deposit(&mut self, memo: &[u8; 512], value: u64, only: Option<&[usize]>) -> ScannedNote {
        let n = self.note(memo, value, Scope::External, 1);
        self.feed(ChainEvent::Note(n.clone()), only);
        n
    }

    pub fn coordinate(&mut self) -> Round {
        let mut s = self.session;
        let r = coordinate(&mut self.nodes, 0, &self.pubkeys.clone(), 2, &mut self.ledger, &mut s, &mut OsRng).unwrap();
        self.session = s;
        r
    }

    pub fn secrets(&self) -> AppSecrets {
        AppSecrets::derive(&MASTER, Network::Testnet, &app_id())
    }
}

pub fn block_hash(height: u32, fork: u8) -> [u8; 32] {
    let mut h = [fork; 32];
    h[..4].copy_from_slice(&height.to_le_bytes());
    h
}

/// A signed deposit memo for a fresh session key.
pub fn memo(sk: &SigningKey, expiry: u32, payout: [u8; 43]) -> DepositMemo {
    let mut m = DepositMemo::new(0, sk.verifying_key().to_bytes(), payout, expiry, [0; 32], b"table=1").unwrap();
    m.set_signature(sk.sign(&m.signing_message(Network::Testnet, &VAULT)).to_bytes());
    m
}
