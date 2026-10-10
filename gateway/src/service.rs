//! The gateway service loop: each operator node pulls its own `zol-zcashd`'s
//! events, then the quorum attests, every node guards, and voided deposits are
//! refunded.
//!
//! `Cluster` runs every operator's node in one process (tests, local devnet).
//! `serve` runs one operator per host; the same rounds go to other operators
//! over authenticated HTTP (`peer`, `server`).

use frost_ed25519::keys::PublicKeyPackage;
use rand::{CryptoRng, RngCore};

use std::collections::BTreeMap;

use zol_ledger::state::AppState;

use crate::chain::AppId;
use crate::ledger::Ledger;
use crate::node::{GuardAction, Node, NodeError, Round};
use crate::payout::Wait;
use crate::peer::{LocalPeer, Peer};
use crate::rounds;
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

    /// Reads this operator's own zcashd head chain for each active app.
    pub fn refresh_heads(&self, ledger: &dyn Ledger) -> Result<(), ServiceError> {
        for (app, rt) in &self.node.apps {
            let Some(acc) = ledger.app(&rt.program).map_err(NodeError::from)? else { continue };
            if !matches!(acc.state, AppState::Active | AppState::Frozen) {
                continue;
            }
            let chain = self.zcashd.head(app, &acc.genesis_head)?;
            self.node.set_head(app, chain)?;
        }
        Ok(())
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

        // Coordinator for this tick: rotates past one that failed last time.
        let n = self.operators.len();
        let c = (self.coordinator + self.attempts.values().copied().max().unwrap_or(0)) % n;
        let net = self.operators[c].node.cfg.network();
        let mut peers: Vec<Box<dyn Peer + '_>> = self
            .operators
            .iter_mut()
            .map(|o| Box::new(LocalPeer { node: &mut o.node, zcashd: Some(&o.zcashd) }) as Box<dyn Peer>)
            .collect();
        if peers[c].proposals(ledger).is_ok() {
            report.round = rounds::attest_rounds(&mut peers, c, ledger, net, &self.pubkeys, self.threshold, rng);
        }
        drop(peers);

        for op in &self.operators {
            for a in op.node.guard(ledger)? {
                report.guard.push((op.node.name.clone(), a));
            }
        }

        let mut peers: Vec<Box<dyn Peer + '_>> = self
            .operators
            .iter_mut()
            .map(|o| Box::new(LocalPeer { node: &mut o.node, zcashd: Some(&o.zcashd) }) as Box<dyn Peer>)
            .collect();
        let spends = rounds::spend_rounds(&mut peers, c, ledger, self.threshold, now, rng);
        drop(peers);
        report.refunds = spends.refunds;
        report.payouts = spends.payouts;
        report.waits = spends.waits;
        for (app, _, _) in &report.payouts {
            self.attempts.insert(*app, 0);
        }
        for (id, e) in spends.failed {
            if e.starts_with("payout") || e.starts_with("propose") {
                *self.attempts.entry(id).or_insert(0) += 1;
            }
            report.round.failed.push((id, e));
        }
        Ok(report)
    }

    fn refresh_heads(&mut self, ledger: &dyn Ledger) -> Result<(), ServiceError> {
        for op in &self.operators {
            op.refresh_heads(ledger)?;
        }
        Ok(())
    }
}
