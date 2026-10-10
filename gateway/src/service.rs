//! The gateway service loop: each operator node pulls its own `zol-zcashd`'s
//! events, then the quorum attests, every node guards, and voided deposits are
//! refunded.
//!
//! In this version the operators' nodes run in one process (a "cluster"), as
//! for devnet and tests. A deployment where each operator runs alone needs an
//! authenticated transport for FROST rounds and chain reports between them.

use frost_ed25519::keys::PublicKeyPackage;
use rand::{CryptoRng, RngCore};

use crate::chain::AppId;
use crate::ledger::Ledger;
use crate::node::{coordinate, GuardAction, Node, NodeError, Round};
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

        // Refunds go through the coordinator's zcashd, which builds, signs (Pallas
        // FROST) and broadcasts; it is idempotent per note while one is pending.
        let coord = &self.operators[self.coordinator];
        if coord.node.halted()?.is_none() {
            let apps: Vec<AppId> = coord.node.apps.keys().copied().collect();
            for app in apps {
                for (note, to) in coord.node.refunds_due(ledger, &app)? {
                    match coord.zcashd.refund(&app, &note.note.cmx, &to) {
                        Ok(r) => report.refunds.push((app, note.note.cmx, r.txid)),
                        Err(e) => report.round.failed.push((note.deposit_id, format!("refund: {e}"))),
                    }
                }
            }
        }
        Ok(report)
    }
}
