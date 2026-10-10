//! The coordinator's work, over peers (in-process or remote): attest deposit
//! decisions and paid records, relay them to the ledger, and sign refunds and
//! payouts with every operator's own zcashd.

use std::collections::BTreeMap;

use frost_ed25519 as frost;
use frost_ed25519::keys::PublicKeyPackage;
use rand::{CryptoRng, RngCore};
use zol_ledger::CreditArgs;
use zol_types::api::{Broadcast, CommitResponse};
use zol_wire::Network;

use crate::attestor::Statement;
use crate::chain::AppId;
use crate::ledger::Ledger;
use crate::node::Round;
use crate::payout::Wait;
use crate::peer::{BroadcastNotice, Peer, PeerError, SpendTarget};

/// Gathers a threshold of Ed25519 FROST shares for `st` from the peers that
/// agree, replacing any that refuse at signing time.
pub fn attest_over_peers<R: RngCore + CryptoRng>(
    peers: &mut [Box<dyn Peer + '_>],
    ledger: &dyn Ledger,
    st: &Statement,
    network: Network,
    pubkeys: &PublicKeyPackage,
    threshold: usize,
    rng: &mut R,
) -> Result<[u8; 64], String> {
    let digest = st.digest(network);
    let mut excluded: Vec<usize> = Vec::new();
    let mut refusals = Vec::new();
    loop {
        let session = rng.next_u64();
        let mut chosen: Vec<(usize, frost::Identifier, frost::round1::SigningCommitments)> = Vec::new();
        for (i, peer) in peers.iter_mut().enumerate() {
            if excluded.contains(&i) || chosen.len() == threshold {
                continue;
            }
            match peer.attest_commit(ledger, session, st) {
                Ok((id, c)) => {
                    let id = frost::Identifier::deserialize(&id).map_err(|e| e.to_string())?;
                    let c = frost::round1::SigningCommitments::deserialize(&c).map_err(|e| e.to_string())?;
                    chosen.push((i, id, c));
                }
                Err(e) => {
                    refusals.push(format!("{}: {e}", peer.name()));
                    excluded.push(i);
                }
            }
        }
        if chosen.len() < threshold {
            return Err(format!("fewer than threshold operators agreed: {}; {refusals:?}", chosen.len()));
        }
        let commitments: BTreeMap<_, _> = chosen.iter().map(|(_, id, c)| (*id, *c)).collect();
        let package = frost::SigningPackage::new(commitments, &digest);
        let pkg_bytes = package.serialize().map_err(|e| e.to_string())?;
        let mut shares = BTreeMap::new();
        let mut dropped = Vec::new();
        for (i, id, _) in &chosen {
            match peers[*i].attest_sign(ledger, session, st, &pkg_bytes) {
                Ok((_, s)) => {
                    shares.insert(*id, frost::round2::SignatureShare::deserialize(&s).map_err(|e| e.to_string())?);
                }
                Err(e) => {
                    refusals.push(format!("{}: {e}", peers[*i].name()));
                    dropped.push(*i);
                }
            }
        }
        if dropped.is_empty() {
            let sig = frost::aggregate(&package, &shares, pubkeys).map_err(|e| e.to_string())?;
            pubkeys.verifying_key().verify(&digest, &sig).map_err(|e| e.to_string())?;
            return Ok(sig.serialize().map_err(|e| e.to_string())?.try_into().expect("64 bytes"));
        }
        excluded.extend(dropped);
    }
}

fn statement_id(st: &Statement) -> [u8; 32] {
    match st {
        Statement::Credit { credit, .. } => credit.deposit_id,
        Statement::Void { deposit_id, .. } => *deposit_id,
        Statement::Paid { sequence, .. } => {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&sequence.to_le_bytes());
            id
        }
    }
}

/// Deposit decisions and paid records: proposed by `peers[c]` from its own view.
pub fn attest_rounds<R: RngCore + CryptoRng>(
    peers: &mut [Box<dyn Peer + '_>],
    c: usize,
    ledger: &mut dyn Ledger,
    network: Network,
    pubkeys: &PublicKeyPackage,
    threshold: usize,
    rng: &mut R,
) -> Round {
    let mut round = Round::default();
    let proposals = match peers[c].proposals(ledger) {
        Ok(p) => p,
        Err(e) => {
            round.failed.push(([0; 32], format!("proposals: {e}")));
            return round;
        }
    };
    for st in proposals.statements {
        let id = statement_id(&st);
        let sig = match attest_over_peers(peers, ledger, &st, network, pubkeys, threshold, rng) {
            Ok(s) => s,
            Err(e) => {
                round.failed.push((id, e));
                continue;
            }
        };
        let Some(program) = peers[c].program(match &st {
            Statement::Credit { app, .. } | Statement::Void { app, .. } | Statement::Paid { app, .. } => app,
        }) else {
            continue;
        };
        let sent = match &st {
            Statement::Credit { credit, .. } => ledger.submit_credit(
                &program,
                CreditArgs {
                    deposit_id: credit.deposit_id,
                    amount: credit.amount,
                    session_key: anchor_lang::prelude::Pubkey::new_from_array(credit.session_key),
                    payout_commitment: credit.payout_commitment,
                    flags: credit.flags,
                    pot_id: credit.pot_id,
                    app_data: credit.app_data.clone(),
                },
                sig,
            ),
            Statement::Void { deposit_id, .. } => ledger.submit_void(&program, *deposit_id, sig),
            Statement::Paid { .. } => continue,
        };
        match (sent, &st) {
            (Err(e), _) => round.failed.push((id, e.to_string())),
            (Ok(()), Statement::Credit { .. }) => round.credited.push(id),
            (Ok(()), _) => round.voided.push(id),
        }
    }
    // mark_paid strictly in order per app; stop an app at its first failure.
    let mut stopped: Vec<AppId> = Vec::new();
    for st in proposals.paid {
        let Statement::Paid { app, sequence } = st else { continue };
        if stopped.contains(&app) {
            continue;
        }
        let Some(program) = peers[c].program(&app) else { continue };
        let r = attest_over_peers(peers, ledger, &st, network, pubkeys, threshold, rng)
            .and_then(|sig| ledger.submit_paid(&program, sequence, sig).map_err(|e| e.to_string()));
        match r {
            Ok(()) => round.marked_paid.push(sequence),
            Err(e) => {
                round.failed.push((statement_id(&st), format!("mark_paid {sequence}: {e}")));
                stopped.push(app);
            }
        }
    }
    for p in peers.iter_mut() {
        let _ = p.sync(ledger);
    }
    round
}

/// Pallas FROST spend signing across peers: each commits only with its own
/// view of `target`; the first `needed` sign; `peers[c]` aggregates and
/// broadcasts; every peer is told so it tracks the spend as pending.
#[allow(clippy::too_many_arguments)]
pub fn spend_over_peers(
    peers: &mut [Box<dyn Peer + '_>],
    c: usize,
    ledger: &dyn Ledger,
    target: &SpendTarget,
    pczt: &[u8],
    needed: usize,
    session: u64,
    now: i64,
) -> Result<Broadcast, String> {
    let mut commits: Vec<(usize, CommitResponse)> = Vec::new();
    let mut refused = Vec::new();
    for (i, peer) in peers.iter_mut().enumerate() {
        if commits.len() == needed {
            break;
        }
        match peer.spend_commit(ledger, session, target, pczt, now) {
            Ok(r) => commits.push((i, r)),
            Err(e) => refused.push(format!("{}: {e}", peer.name())),
        }
    }
    if commits.len() < needed {
        return Err(format!("{} of {needed} operators committed; refused: {refused:?}", commits.len()));
    }
    let commitments: Vec<CommitResponse> = commits.iter().map(|(_, c)| c.clone()).collect();
    let mut shares = Vec::new();
    for (i, _) in &commits {
        shares.push(peers[*i].spend_share(session, &commitments).map_err(|e| format!("{}: share: {e}", peers[*i].name()))?);
    }
    let b = peers[c].aggregate(session, pczt, commitments, shares).map_err(|e| format!("aggregate: {e}"))?;
    let notice = BroadcastNotice { target: target.clone(), txid: b.txid, expiry_height: b.expiry_height };
    for p in peers.iter_mut() {
        let _ = p.note_broadcast(ledger, &notice, now);
    }
    Ok(b)
}

#[derive(Debug, Default)]
pub struct SpendReport {
    pub refunds: Vec<(AppId, [u8; 32], [u8; 32])>,
    pub payouts: Vec<(AppId, [u8; 32], usize)>,
    pub waits: Vec<(AppId, Wait)>,
    pub failed: Vec<([u8; 32], String)>,
}

/// Refunds then payouts, coordinated by `peers[c]`.
#[allow(clippy::too_many_arguments)]
pub fn spend_rounds<R: RngCore + CryptoRng>(
    peers: &mut [Box<dyn Peer + '_>],
    c: usize,
    ledger: &dyn Ledger,
    threshold: usize,
    now: i64,
    rng: &mut R,
) -> SpendReport {
    let mut rep = SpendReport::default();
    match peers[c].refund_proposals(ledger) {
        Ok(list) => {
            for (target, proposal) in list {
                let SpendTarget::Refund { app, cmx, .. } = &target else { continue };
                match spend_over_peers(peers, c, ledger, &target, &proposal.pczt, threshold, rng.next_u64(), now) {
                    Ok(b) => rep.refunds.push((*app, *cmx, b.txid)),
                    Err(e) => rep.failed.push((*cmx, format!("refund: {e}"))),
                }
            }
        }
        Err(PeerError::NotLocal(_)) => {}
        Err(e) => rep.failed.push(([0; 32], format!("refund proposals: {e}"))),
    }
    let n = peers.len();
    for app in peers[c].app_ids() {
        match peers[c].payout_proposal(ledger, &app, now) {
            Ok(Some((planned, proposal))) => {
                let needed = if planned.needs_all { n } else { threshold };
                let target = SpendTarget::Payout { app };
                match spend_over_peers(peers, c, ledger, &target, &proposal.pczt, needed, rng.next_u64(), now) {
                    Ok(b) => rep.payouts.push((app, b.txid, planned.plan.records.len())),
                    Err(e) => rep.failed.push((app, format!("payout via {}: {e}", peers[c].name()))),
                }
            }
            Ok(None) => {
                if let Some(w) = peers[c].wait_reason(ledger, &app, now) {
                    rep.waits.push((app, w));
                }
            }
            Err(PeerError::NotLocal(_)) => {}
            Err(e) => rep.failed.push((app, format!("propose by {}: {e}", peers[c].name()))),
        }
    }
    rep
}

/// Chain agreement across peers: every local node checks every report.
pub fn chain_reports(peers: &mut [Box<dyn Peer + '_>]) -> Vec<(u32, [u8; 32])> {
    peers.iter_mut().filter_map(|p| p.chain_report().ok().flatten()).collect()
}
