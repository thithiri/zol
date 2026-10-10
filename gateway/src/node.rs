//! One operator's gateway node, and a coordinator that drives a quorum of them.
//!
//! Every node runs its own scanner, its own store and its own ledger reads, and
//! decides independently. The coordinator only proposes; each node signs only
//! what its own view produces (plan 3.4–3.7, 3.9; design "Attestation and
//! credit", "Signer rules", "Confirmation policy").

use std::collections::BTreeMap;

use anchor_lang::prelude::Pubkey;
use frost_ed25519::keys::PublicKeyPackage;
use rand::{CryptoRng, RngCore};
use zol_ledger::state::{App, AppState, Config, DepositState};
use zol_ledger::CreditArgs;
use zol_wire::memo::classify;
use zol_wire::{DepositMemo, HeadMemo, MemoKind, Network, MEMO_LEN, ORCHARD_ADDR_LEN};

use crate::attestor::{attest, AttestError, Attestor, Participant, Statement};
use crate::chain::{AddressCheck, AppId, ChainEvent, Scope};
use crate::config::{AppSecrets, GatewayConfig};
use crate::decide::{decide, AppStatus, DecideCtx, Decision};
use crate::ledger::{Ledger, LedgerError};
use crate::policy::{confirmations, depth, early_credit_allowed, Depth};
use crate::reconcile::{compute_reserve, pending_early, reconcile, Mismatch, ReconcileCtx};
use crate::store::{NoteKind, NoteRecord, NoteState, Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("ledger: {0}")]
    Ledger(#[from] LedgerError),
    #[error("attest: {0}")]
    Attest(#[from] AttestError),
    #[error("halted: {0}")]
    Halted(String),
    #[error("bad intent: {0}")]
    BadIntent(&'static str),
}

pub type Result<T> = std::result::Result<T, NodeError>;

/// Why credits and payouts stopped. Sticky until operators clear it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HaltReason {
    /// A block with at least full-depth confirmations was disconnected.
    DeepReorg { from_height: u32, tip: u32 },
    /// A credited deposit at full depth was rolled back.
    FullDepthCreditRolledBack { deposit_id: [u8; 32] },
    /// Claims plus fee pool exceed the reserve: a rolled-back early credit was larger than the float.
    ReserveShort { app: AppId },
    /// Operators' nodes disagree on a block at early depth.
    ChainDisagreement { height: u32 },
    /// No new tip for too long.
    TipStalled { since: i64 },
}

impl std::fmt::Display for HaltReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Maps a session key to the diversifier index of its vault receiver.
pub type SessionIndex = Box<dyn Fn(&[u8; 32]) -> u128>;

/// Per-app runtime data on a node.
pub struct AppRuntime {
    pub program: Pubkey,
    /// The address the session key signs (index 0, external).
    pub vault_address: [u8; ORCHARD_ADDR_LEN],
    pub secrets: AppSecrets,
    /// Diversifier index of a session's receiver (from `zol-zcash` keys).
    pub session_index: SessionIndex,
}

pub struct Node {
    pub name: String,
    pub store: Store,
    pub cfg: GatewayConfig,
    pub attestor: Attestor,
    /// This operator's guardian key (Ed25519 seed of its Solana keypair).
    pub guardian_seed: [u8; 32],
    pub apps: BTreeMap<AppId, AppRuntime>,
    pub addresses: Box<dyn AddressCheck>,
}

/// What a node did on a reconciliation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardAction {
    Froze { app: AppId, mismatches: Vec<Mismatch> },
    CancelledActivation { app: AppId, problem: GenesisProblem },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenesisProblem {
    Missing,
    NotInternal,
    NotGenesisMemo,
    Shallow { confirmations: u32 },
}

fn app_status(app: &App, cfg: &Config, now: i64) -> AppStatus {
    let active = match app.state {
        AppState::Active => true,
        AppState::Activating => now >= app.activates_at,
        _ => false,
    };
    AppStatus {
        accepting: active && !cfg.paused,
        entry_fee_bps: cfg.params.entry_fee_bps,
        cap_room: app.deposit_cap.saturating_sub(app.liability).saturating_sub(app.unpaid_total),
    }
}

impl Node {
    fn network(&self) -> Network {
        self.cfg.network()
    }

    pub fn halted(&self) -> Result<Option<String>> {
        Ok(self.store.meta("halt")?.map(|v| String::from_utf8_lossy(&v).into_owned()))
    }

    pub fn halt(&self, reason: HaltReason) -> Result<()> {
        if self.halted()?.is_none() {
            self.store.set_meta("halt", reason.to_string().as_bytes())?;
        }
        Ok(())
    }

    /// Operators clear a halt by hand after recovery.
    pub fn clear_halt(&self) -> Result<()> {
        Ok(self.store.del_meta("halt")?)
    }

    fn require_running(&self) -> Result<()> {
        match self.halted()? {
            Some(r) => Err(NodeError::Halted(r)),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------ chain input

    /// Applies one scanner event. `now` is wall-clock time for stall detection.
    pub fn apply(&self, ev: &ChainEvent, now: i64) -> Result<()> {
        match ev {
            ChainEvent::Block { height, hash } => {
                self.store.put_block(*height, hash)?;
                self.store.set_meta("tip_time", &now.to_le_bytes())?;
            }
            ChainEvent::Note(n) => {
                let Some(rt) = self.apps.get(&n.app) else { return Ok(()) };
                let kind = match classify(&crate::chain::memo_array(n)) {
                    Ok(MemoKind::HeadV1) => NoteKind::Head,
                    Ok(MemoKind::TopUpV1) => NoteKind::TopUp,
                    _ => NoteKind::Deposit,
                };
                self.store.insert_note(n, kind, &rt.secrets.deposit_id(&n.cmx))?;
            }
            ChainEvent::Spent { app, nullifier, txid, height } => {
                self.store.mark_spent(app, nullifier, txid, *height)?;
            }
            ChainEvent::Rewind { to_height } => {
                let tip = self.store.tip()?.unwrap_or(*to_height);
                // Disconnecting a block that had full-depth confirmations.
                if confirmations(to_height + 1, tip) >= self.cfg.depths.full {
                    self.halt(HaltReason::DeepReorg { from_height: to_height + 1, tip })?;
                }
                for n in self.store.rewind(*to_height)? {
                    if n.state == NoteState::Credited && depth(n.note.height, tip, &self.cfg.depths) == Depth::Full {
                        self.halt(HaltReason::FullDepthCreditRolledBack { deposit_id: n.deposit_id })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// This node's report for chain agreement: the block it holds at early
    /// depth below its tip. Operators exchange these (signed with their guardian
    /// keys) every round.
    pub fn chain_report(&self) -> Result<Option<(u32, [u8; 32])>> {
        let Some(tip) = self.store.tip()? else { return Ok(None) };
        let h = tip.saturating_sub(self.cfg.depths.early - 1);
        Ok(self.store.block(h)?.map(|hash| (h, hash)))
    }

    /// Halts if a peer reports a different block at a height this node also
    /// holds at early depth or deeper (design "Global halt": signers' nodes
    /// disagree on the chain).
    pub fn check_agreement(&self, peer_reports: &[(u32, [u8; 32])]) -> Result<()> {
        let Some(tip) = self.store.tip()? else { return Ok(()) };
        for (h, hash) in peer_reports {
            if confirmations(*h, tip) < self.cfg.depths.early {
                continue;
            }
            if let Some(mine) = self.store.block(*h)? {
                if mine != *hash {
                    self.halt(HaltReason::ChainDisagreement { height: *h })?;
                }
            }
        }
        Ok(())
    }

    /// Halts if the tip has not moved for `tip_stall_secs`.
    pub fn check_stall(&self, now: i64) -> Result<()> {
        if let Some(t) = self.store.meta("tip_time")? {
            let last = i64::from_le_bytes(t.try_into().unwrap_or([0; 8]));
            if now - last > self.cfg.tip_stall_secs {
                self.halt(HaltReason::TipStalled { since: last })?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ intents

    /// Registers a deposit memo off-chain (from the app page), so a note that
    /// reaches the session's receiver without its memo can still be credited.
    pub fn register_intent(&self, app: &AppId, memo: &[u8; MEMO_LEN]) -> Result<u128> {
        let rt = self.apps.get(app).ok_or(NodeError::BadIntent("unknown app"))?;
        let m = DepositMemo::decode_any(memo).map_err(|_| NodeError::BadIntent("not a deposit memo"))?;
        m.verify_signature(self.network(), &rt.vault_address)
            .map_err(|_| NodeError::BadIntent("bad session signature"))?;
        let idx = (rt.session_index)(&m.session_key);
        self.store.put_intent(app, idx, &m.session_key, &m.encode())?;
        Ok(idx)
    }

    fn intent(&self, app: &AppId, idx: u128) -> Option<[u8; MEMO_LEN]> {
        self.store.intent(app, idx).ok().flatten().and_then(|v| v.try_into().ok())
    }

    // ------------------------------------------------------------ decisions

    /// Brings note states in line with what is already on the ledger (decisions
    /// other operators' quorums signed).
    pub fn sync_with_ledger(&self, ledger: &dyn Ledger, app: &AppId) -> Result<()> {
        for n in self.store.notes(app)? {
            if n.kind != NoteKind::Deposit || matches!(n.state, NoteState::Credited | NoteState::Voided) {
                continue;
            }
            if let Some(d) = ledger.deposit(&n.deposit_id)? {
                let state = if d.state == DepositState::Credited { NoteState::Credited } else { NoteState::Voided };
                let memo = crate::decide::resolve_memo(&n.note, self.intent(app, n.note.receiver_index).as_ref())
                    .map(|(_, canon, _)| canon.to_vec());
                let tip = self.store.tip()?.unwrap_or(0);
                let c = Some(confirmations(n.note.height, tip));
                self.store.set_decision(app, &n.note.cmx, state, memo.as_deref(), Some(&d.digest), c)?;
            }
        }
        Ok(())
    }

    /// The statement this node would sign for each undecided deposit of `app`
    /// that has reached early depth. Credits short of full depth must fit the
    /// per-deposit cap and the remaining float.
    pub fn own_statements(&self, ledger: &dyn Ledger, app: &AppId) -> Result<Vec<(NoteRecord, Statement, Decision)>> {
        self.require_running()?;
        let rt = &self.apps[app];
        let Some(tip) = self.store.tip()? else { return Ok(vec![]) };
        let Some(app_acc) = ledger.app(&rt.program)? else { return Ok(vec![]) };
        let cfg = ledger.config()?;
        let status = app_status(&app_acc, &cfg, ledger.now()?);
        let notes = self.store.notes(app)?;
        let reserve = compute_reserve(&app_acc, &notes, tip, &self.cfg.depths);
        let mut pending = pending_early(&notes, tip, &self.cfg.depths);
        let mut out = Vec::new();
        for n in notes {
            if n.kind != NoteKind::Deposit || n.rolled_back || !matches!(n.state, NoteState::Seen | NoteState::Held) {
                continue;
            }
            let d = depth(n.note.height, tip, &self.cfg.depths);
            if d == Depth::Unconfirmed || ledger.deposit(&n.deposit_id)?.is_some() {
                continue;
            }
            let intent = self.intent(app, n.note.receiver_index);
            let existing = match crate::decide::resolve_memo(&n.note, intent.as_ref()) {
                Some((m, _, _)) => ledger.balance(&rt.program, &Pubkey::new_from_array(m.session_key))?.map(|b| b.payout_commitment),
                None => None,
            };
            let ctx = DecideCtx {
                app: *app,
                network: self.network(),
                vault_address: rt.vault_address,
                tip,
                denominations: &self.cfg.denominations,
                secrets: &rt.secrets,
                addresses: self.addresses.as_ref(),
                status,
                intent,
                existing_commitment: existing,
            };
            let decision = decide(&n.note, &ctx);
            let st = match &decision {
                Decision::Credit { credit, .. } => {
                    if d < Depth::Full {
                        if !early_credit_allowed(credit.amount, pending, self.cfg.early_credit_cap, &reserve) {
                            continue; // wait for full depth
                        }
                        pending += credit.amount as u128;
                    }
                    Statement::Credit { app: *app, credit: credit.clone() }
                }
                Decision::Void { deposit_id, .. } => Statement::Void { app: *app, deposit_id: *deposit_id },
                Decision::Hold { .. } => {
                    if n.state != NoteState::Held {
                        self.store.set_decision(app, &n.note.cmx, NoteState::Held, None, None, None)?;
                    }
                    continue;
                }
                Decision::NotDeposit(_) => continue,
            };
            out.push((n, st, decision));
        }
        Ok(out)
    }

    /// Records locally that this node's quorum signed `decision` for `note`.
    fn mark_decided(&self, note: &NoteRecord, decision: &Decision, digest: &[u8; 32]) -> Result<()> {
        let tip = self.store.tip()?.unwrap_or(0);
        let (state, memo) = match decision {
            Decision::Credit { memo, .. } => (NoteState::Credited, memo),
            Decision::Void { memo, .. } => (NoteState::Voided, memo),
            _ => return Ok(()),
        };
        self.store.set_decision(
            &note.note.app,
            &note.note.cmx,
            state,
            Some(&memo[..]),
            Some(digest),
            Some(confirmations(note.note.height, tip)),
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ guardian duties

    /// Reconciles every app and freezes those that do not match. Also cancels
    /// an activation whose genesis head this node cannot confirm. Halts if an
    /// app's reserve is short.
    pub fn guard(&self, ledger: &mut dyn Ledger) -> Result<Vec<GuardAction>> {
        let mut actions = Vec::new();
        let Some(tip) = self.store.tip()? else { return Ok(actions) };
        let now = ledger.now()?;
        for (app, rt) in &self.apps {
            let Some(acc) = ledger.app(&rt.program)? else { continue };
            self.sync_with_ledger(ledger, app)?;
            let notes = self.store.notes(app)?;
            match acc.state {
                AppState::Activating if now < acc.activates_at => {
                    if let Err(problem) = check_genesis(&acc, app, &notes, tip, self) {
                        ledger.freeze(&self.guardian_seed, &rt.program)?;
                        actions.push(GuardAction::CancelledActivation { app: *app, problem });
                    }
                }
                AppState::Active | AppState::Activating => {
                    let deposits = ledger.deposits(&rt.program)?;
                    let intents = |idx: u128| self.intent(app, idx);
                    let ctx = ReconcileCtx {
                        network: self.network(),
                        vault_address: rt.vault_address,
                        secrets: &rt.secrets,
                        tip,
                        depths: &self.cfg.depths,
                    };
                    let mismatches = reconcile(&acc, &deposits, &notes, &intents, &ctx);
                    if compute_reserve(&acc, &notes, tip, &self.cfg.depths).is_short() {
                        self.halt(HaltReason::ReserveShort { app: *app })?;
                    }
                    if !mismatches.is_empty() {
                        ledger.freeze(&self.guardian_seed, &rt.program)?;
                        actions.push(GuardAction::Froze { app: *app, mismatches });
                    }
                }
                _ => {}
            }
        }
        Ok(actions)
    }

    /// Voided deposits whose refund is due: the void is on the ledger and the
    /// deposited note is unspent in this node's scan (plan 3.8).
    pub fn refunds_due(&self, ledger: &dyn Ledger, app: &AppId) -> Result<Vec<(NoteRecord, [u8; ORCHARD_ADDR_LEN])>> {
        let mut out = Vec::new();
        for n in self.store.notes(app)? {
            if n.kind != NoteKind::Deposit || n.rolled_back || n.spent.is_some() {
                continue;
            }
            let Some(d) = ledger.deposit(&n.deposit_id)? else { continue };
            if d.state != DepositState::Voided {
                continue;
            }
            let memo = n
                .decided_memo
                .as_ref()
                .and_then(|m| <[u8; 512]>::try_from(m.as_slice()).ok())
                .and_then(|m| DepositMemo::decode(&m).ok());
            if let Some(m) = memo {
                out.push((n, m.payout_address));
            }
        }
        Ok(out)
    }
}

/// Guardians cancel an activation unless the genesis head exists in this
/// node's scan at full depth, in the vault's internal scope, with a genesis memo
/// for this app (design "The vault head").
pub fn check_genesis(app: &App, app_id: &AppId, notes: &[NoteRecord], tip: u32, node: &Node) -> std::result::Result<(), GenesisProblem> {
    let n = notes.iter().find(|n| n.note.cmx == app.genesis_head && !n.rolled_back).ok_or(GenesisProblem::Missing)?;
    if n.note.scope != Scope::Internal {
        return Err(GenesisProblem::NotInternal);
    }
    match HeadMemo::decode(&crate::chain::memo_array(&n.note)) {
        Ok(m) if m.is_genesis() && m.app == *app_id && m.network == node.network() => {}
        _ => return Err(GenesisProblem::NotGenesisMemo),
    }
    let c = confirmations(n.note.height, tip);
    if c < node.cfg.depths.full {
        return Err(GenesisProblem::Shallow { confirmations: c });
    }
    Ok(())
}

/// Report of one coordinator round.
#[derive(Debug, Default)]
pub struct Round {
    pub credited: Vec<[u8; 32]>,
    pub voided: Vec<[u8; 32]>,
    pub failed: Vec<([u8; 32], String)>,
}

/// Proposes every statement the coordinator node sees, gathers a threshold of
/// agreeing nodes, and relays the aggregate signature to the ledger.
pub fn coordinate<R: RngCore + CryptoRng>(
    nodes: &mut [Node],
    coordinator: usize,
    pubkeys: &PublicKeyPackage,
    threshold: usize,
    ledger: &mut dyn Ledger,
    session: &mut u64,
    rng: &mut R,
) -> Result<Round> {
    let mut round = Round::default();
    let app_ids: Vec<AppId> = nodes[coordinator].apps.keys().copied().collect();
    for app in app_ids {
        for n in nodes.iter() {
            n.sync_with_ledger(ledger, &app)?;
        }
        let proposals = nodes[coordinator].own_statements(ledger, &app)?;
        for (_, statement, _) in proposals {
            // Each node's own view of this deposit.
            let key = statement.log_key();
            let mut views = Vec::new();
            for n in nodes.iter() {
                let own = match n.own_statements(ledger, &app) {
                    Ok(v) => v.into_iter().find(|(_, s, _)| s.log_key() == key),
                    Err(_) => None,
                };
                views.push(own);
            }
            *session += 1;
            let net = nodes[coordinator].network();
            let program = nodes[coordinator].apps[&app].program;
            let result = {
                let mut parts: Vec<Participant> = nodes
                    .iter_mut()
                    .zip(&views)
                    .map(|(n, v)| Participant { attestor: &mut n.attestor, log: &n.store, own_view: v.as_ref().map(|(_, s, _)| s.clone()) })
                    .collect();
                attest(&statement, net, pubkeys, threshold, &mut parts, *session, rng)
            };
            let id = match &statement {
                Statement::Credit { credit, .. } => credit.deposit_id,
                Statement::Void { deposit_id, .. } => *deposit_id,
                Statement::Paid { .. } => [0; 32],
            };
            let sig = match result {
                Ok((sig, _)) => sig,
                Err(e) => {
                    round.failed.push((id, e.to_string()));
                    continue;
                }
            };
            let sent = match &statement {
                Statement::Credit { credit, .. } => ledger.submit_credit(
                    &program,
                    CreditArgs {
                        deposit_id: credit.deposit_id,
                        amount: credit.amount,
                        session_key: Pubkey::new_from_array(credit.session_key),
                        payout_commitment: credit.payout_commitment,
                        flags: credit.flags,
                        pot_id: credit.pot_id,
                        app_data: credit.app_data.clone(),
                    },
                    sig,
                ),
                Statement::Void { deposit_id, .. } => ledger.submit_void(&program, *deposit_id, sig),
                Statement::Paid { sequence, .. } => ledger.submit_paid(&program, *sequence, sig),
            };
            if let Err(e) = sent {
                round.failed.push((id, e.to_string()));
                continue;
            }
            let digest = statement.digest(net);
            for (n, v) in nodes.iter().zip(&views) {
                if let Some((note, s, d)) = v {
                    if *s == statement {
                        n.mark_decided(note, d, &digest)?;
                    }
                }
            }
            match statement {
                Statement::Credit { .. } => round.credited.push(id),
                Statement::Void { .. } => round.voided.push(id),
                Statement::Paid { .. } => {}
            }
        }
    }
    Ok(round)
}
