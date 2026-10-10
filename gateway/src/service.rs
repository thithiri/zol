//! The gateway service loop: each operator node pulls its own `zol-zcashd`'s
//! events, then the quorum attests, every node guards, and voided deposits are
//! refunded.
//!
//! In this version the operators' nodes run in one process (a "cluster"), as
//! for devnet and tests. A deployment where each operator runs alone needs an
//! authenticated transport for FROST rounds and chain reports between them.

use frost_ed25519::keys::PublicKeyPackage;
use rand::{CryptoRng, RngCore};

use std::collections::BTreeMap;

use zol_ledger::state::AppState;
use zol_types::api::{AggregateRequest, Broadcast, CommitRequest, CommitResponse, ShareRequest, SignKind};

use crate::chain::AppId;
use crate::ledger::Ledger;
use crate::node::{coordinate, GuardAction, Node, NodeError, Round};
use crate::payout::{self, Wait};
use crate::zcashd::{ZcashdClient, ZcashdError};

pub struct Operator {
    pub node: Node,
    pub zcashd: ZcashdClient,
    /// Events up to this sequence number are applied. Persisted in the node's store.
    pub cursor: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Node(#[from] NodeError),
    #[error(transparent)]
    Zcashd(#[from] ZcashdError),
}

#[derive(Debug, Default)]
pub struct TickReport {
    pub events: usize,
    pub round: Round,
    pub guard: Vec<(String, GuardAction)>,
    pub refunds: Vec<(AppId, [u8; 32], [u8; 32])>,
    /// Head transitions broadcast this tick: (app, txid, records paid).
    pub payouts: Vec<(AppId, [u8; 32], usize)>,
    /// Why the coordinator did not propose a payout, per app.
    pub waits: Vec<(AppId, Wait)>,
    pub halted: Vec<(String, String)>,
}

impl Operator {
    pub fn new(node: Node, zcashd: ZcashdClient) -> Result<Self, ServiceError> {
        let cursor = node
            .store
            .meta("zcashd_cursor")
            .map_err(NodeError::from)?
            .map(|v| u64::from_le_bytes(v.try_into().unwrap_or([0; 8])))
            .unwrap_or(0);
        Ok(Self { node, zcashd, cursor })
    }

    /// Applies every new scanner event. Returns how many were applied.
    pub fn pull(&mut self, now: i64) -> Result<usize, ServiceError> {
        let (events, next) = self.zcashd.drain(self.cursor)?;
        for ev in &events {
            self.node.apply(ev, now)?;
        }
        self.cursor = next;
        self.node.store.set_meta("zcashd_cursor", &next.to_le_bytes()).map_err(NodeError::from)?;
        Ok(events.len())
    }
}

pub struct Cluster {
    pub operators: Vec<Operator>,
    pub pubkeys: PublicKeyPackage,
    pub threshold: usize,
    pub coordinator: usize,
    pub session: u64,
    /// Failed payout attempts per app; the payout coordinator rotates with them.
    pub attempts: BTreeMap<AppId, usize>,
}

/// Distributed Pallas FROST spend signing: every operator's own zcashd
/// verifies the PCZT against that operator's own view (`kind_for`) before
/// committing; the first `needed` that commit sign; the coordinator's zcashd
/// aggregates, verifies and broadcasts.
pub fn sign_distributed(
    operators: &[Operator],
    coordinator: usize,
    kind_for: &dyn Fn(usize) -> Option<SignKind>,
    pczt: &[u8],
    needed: usize,
    session: u64,
) -> Result<Broadcast, String> {
    let mut commits: Vec<(usize, CommitResponse)> = Vec::new();
    let mut refused = Vec::new();
    for (i, op) in operators.iter().enumerate() {
        let Some(kind) = kind_for(i) else {
            refused.push(format!("{}: no own view", op.node.name));
            continue;
        };
        match op.zcashd.sign_commit(&CommitRequest { session, kind, pczt: pczt.to_vec() }) {
            Ok(c) => commits.push((i, c)),
            Err(e) => refused.push(format!("{}: {e}", op.node.name)),
        }
        if commits.len() == needed {
            break;
        }
    }
    if commits.len() < needed {
        return Err(format!("{} of {needed} operators committed; refused: {refused:?}", commits.len()));
    }
    let commitments: Vec<CommitResponse> = commits.iter().map(|(_, c)| c.clone()).collect();
    let mut shares = Vec::new();
    for (i, _) in &commits {
        let s = operators[*i]
            .zcashd
            .sign_share(&ShareRequest { session, commitments: commitments.clone() })
            .map_err(|e| format!("{}: share: {e}", operators[*i].node.name))?;
        shares.push(s);
    }
    operators[coordinator]
        .zcashd
        .sign_aggregate(&AggregateRequest { session, pczt: pczt.to_vec(), commitments, shares })
        .map_err(|e| format!("aggregate: {e}"))
}

impl Cluster {
    /// One service round.
    pub fn tick<R: RngCore + CryptoRng>(&mut self, ledger: &mut dyn Ledger, now: i64, rng: &mut R) -> Result<TickReport, ServiceError> {
        let mut report = TickReport::default();
        for op in &mut self.operators {
            report.events += op.pull(now)?;
            op.node.check_stall(now)?;
        }
        // Chain agreement: every node checks every other node's report.
        let reports: Vec<(u32, [u8; 32])> =
            self.operators.iter().filter_map(|o| o.node.chain_report().ok().flatten()).collect();
        for op in &self.operators {
            op.node.check_agreement(&reports)?;
        }
        for op in &self.operators {
            if let Some(h) = op.node.halted()? {
                report.halted.push((op.node.name.clone(), h));
            }
        }

        self.refresh_heads(ledger)?;

        let mut nodes: Vec<Node> = Vec::new();
        let mut rest = Vec::new();
        for op in self.operators.drain(..) {
            nodes.push(op.node);
            rest.push((op.zcashd, op.cursor));
        }
        let round = coordinate(&mut nodes, self.coordinator, &self.pubkeys, self.threshold, ledger, &mut self.session, rng);
        self.operators = nodes
            .into_iter()
            .zip(rest)
            .map(|(node, (zcashd, cursor))| Operator { node, zcashd, cursor })
            .collect();
        match round {
            Ok(r) => report.round = r,
            Err(NodeError::Halted(_)) => {}
            Err(e) => return Err(e.into()),
        }

        for op in &self.operators {
            for a in op.node.guard(ledger)? {
                report.guard.push((op.node.name.clone(), a));
            }
        }

        self.refunds(ledger, &mut report, rng)?;
        self.payouts(ledger, now, &mut report, rng)?;
        Ok(report)
    }

    /// Every node reads its own zcashd's head chain for each active app.
    fn refresh_heads(&mut self, ledger: &dyn Ledger) -> Result<(), ServiceError> {
        for op in &self.operators {
            for (app, rt) in &op.node.apps {
                let Some(acc) = ledger.app(&rt.program).map_err(NodeError::from)? else { continue };
                if !matches!(acc.state, AppState::Active | AppState::Frozen) {
                    continue;
                }
                let chain = op.zcashd.head(app, &acc.genesis_head)?;
                op.node.set_head(app, chain)?;
            }
        }
        Ok(())
    }

    /// Refunds of voided deposits through distributed signing. Each operator
    /// commits only if its own view lists the same refund.
    fn refunds<R: RngCore + CryptoRng>(&mut self, ledger: &dyn Ledger, report: &mut TickReport, rng: &mut R) -> Result<(), ServiceError> {
        let c = self.coordinator;
        if self.operators[c].node.halted()?.is_some() {
            return Ok(());
        }
        let apps: Vec<AppId> = self.operators[c].node.apps.keys().copied().collect();
        for app in apps {
            let due = self.operators[c].node.refunds_due(ledger, &app)?;
            for (note, to) in due {
                let key = format!("refund_pending:{}", hex::encode(note.note.cmx));
                let tip = self.operators[c].node.store.tip().map_err(NodeError::from)?.unwrap_or(0);
                if let Some(v) = self.operators[c].node.store.meta(&key).map_err(NodeError::from)? {
                    if tip < u32::from_le_bytes(v.try_into().unwrap_or([0; 4])) {
                        continue; // broadcast, not yet mined or expired
                    }
                }
                let proposal = match self.operators[c].zcashd.refund_propose(&app, &note.note.cmx, &to) {
                    Ok(p) => p,
                    Err(e) => {
                        report.round.failed.push((note.deposit_id, format!("refund propose: {e}")));
                        continue;
                    }
                };
                let views: Vec<bool> = self
                    .operators
                    .iter()
                    .map(|o| o.node.refunds_due(ledger, &app).map(|v| v.iter().any(|(n, t)| n.note.cmx == note.note.cmx && *t == to)).unwrap_or(false))
                    .collect();
                let kind_for = |i: usize| views[i].then(|| SignKind::Refund { app, cmx: note.note.cmx, to: to.to_vec() });
                let session = rng.next_u64();
                match sign_distributed(&self.operators, c, &kind_for, &proposal.pczt, self.threshold, session) {
                    Ok(b) => {
                        self.operators[c].node.store.set_meta(&key, &b.expiry_height.to_le_bytes()).map_err(NodeError::from)?;
                        report.refunds.push((app, note.note.cmx, b.txid));
                    }
                    Err(e) => report.round.failed.push((note.deposit_id, format!("refund: {e}"))),
                }
            }
        }
        Ok(())
    }

    /// The next head transition per app: the rotating coordinator proposes from
    /// its plan; each operator verifies against its own plan and signs.
    fn payouts<R: RngCore + CryptoRng>(&mut self, ledger: &dyn Ledger, now: i64, report: &mut TickReport, rng: &mut R) -> Result<(), ServiceError> {
        let n = self.operators.len();
        let apps: Vec<AppId> = self.operators[self.coordinator].node.apps.keys().copied().collect();
        for app in apps {
            let attempt = *self.attempts.get(&app).unwrap_or(&0);
            let c = (self.coordinator + attempt) % n;
            let planned = match payout::plan(&self.operators[c].node, ledger, &app, now) {
                Ok(Ok(p)) => p,
                Ok(Err(w)) => {
                    report.waits.push((app, w));
                    continue;
                }
                Err(NodeError::Halted(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            let proposal = match self.operators[c].zcashd.payout_propose(&planned.plan) {
                Ok(p) => p,
                Err(e) => {
                    report.round.failed.push((app, format!("propose by {}: {e}", self.operators[c].node.name)));
                    *self.attempts.entry(app).or_insert(0) += 1;
                    continue;
                }
            };
            let own: Vec<Option<payout::Planned>> =
                self.operators.iter().map(|o| payout::plan(&o.node, ledger, &app, now).ok().and_then(|r| r.ok())).collect();
            let kind_for = |i: usize| own[i].as_ref().map(|p| SignKind::Payout { plan: p.plan.clone() });
            let needed = if planned.needs_all { n } else { self.threshold };
            let session = rng.next_u64();
            match sign_distributed(&self.operators, c, &kind_for, &proposal.pczt, needed, session) {
                Ok(b) => {
                    for (op, p) in self.operators.iter().zip(&own) {
                        if let Some(p) = p {
                            payout::record_broadcast(&op.node, &app, p, &b.txid, b.expiry_height, now)?;
                        }
                    }
                    report.payouts.push((app, b.txid, planned.plan.records.len()));
                    self.attempts.insert(app, 0);
                }
                Err(e) => {
                    report.round.failed.push((app, format!("payout via {}: {e}", self.operators[c].node.name)));
                    *self.attempts.entry(app).or_insert(0) += 1;
                }
            }
        }
        Ok(())
    }
}
