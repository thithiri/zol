//! Payout plans (plan 4.x; design "Exit: settlement and payout", signer rules
//! 1, 4, 5, 6, 8).
//!
//! Every operator computes its own `PayoutPlan` from its own finalized ledger
//! reads, its own scan and its own head chain. Its zcashd verifies the
//! coordinator's proposal against that plan, never against the coordinator's.

use std::collections::BTreeMap;

use zol_ledger::state::{AppState, DepositState, SettlementState};
use zol_types::api::{HeadChain, PayoutLine, PayoutPlan, PayoutRecord};
use zol_wire::DepositMemo;

use crate::chain::AppId;
use crate::ledger::Ledger;
use crate::node::{Node, NodeError, Result};
use crate::reconcile::{reconcile, ReconcileCtx};
use crate::store::{NoteKind, NoteState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// No records waiting and nothing to consolidate.
    Nothing,
    /// A head transition is broadcast and not yet mined or expired.
    Pending([u8; 32]),
    AppNotActive,
    /// Rule 4: reconciliation does not match.
    Unreconciled,
    /// Rule 5: a payout commitment does not map to an address in this scan.
    Unresolved { sequence: u64, commitment: [u8; 32] },
    /// Rule 8: a line above the per-payout cap is still inside its delay.
    LargeDelay { sequence: u64 },
    /// Rule 8: the rolling hourly cap would be exceeded.
    HourlyCap,
    /// Schedule: too soon after the last payout.
    Schedule,
    NoHead,
    /// The padded transition's ZIP 317 fee exceeds what is left in the fee pool.
    /// Raise the ledger's network fee allowance to at least 5,000 × the smallest
    /// action size, or top up the fee pool.
    FeePool { need: u64, have: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub plan: PayoutPlan,
    /// A line above the per-payout cap: every operator must sign.
    pub needs_all: bool,
    pub total: u64,
}

fn meta_key(kind: &str, app: &AppId) -> String {
    format!("{kind}:{}", hex::encode(app))
}

fn meta_i64(node: &Node, k: &str) -> Result<Option<i64>> {
    Ok(node.store.meta(k)?.map(|v| i64::from_le_bytes(v.try_into().unwrap_or([0; 8]))))
}

/// The payout address behind `commitment`: the payout address of a credited
/// deposit of this app whose memo gives exactly this commitment.
pub fn resolve_address(node: &Node, app: &AppId, commitment: &[u8; 32]) -> Result<Option<[u8; 43]>> {
    let rt = &node.apps[app];
    for n in node.store.notes(app)? {
        if n.kind != NoteKind::Deposit || n.state != NoteState::Credited {
            continue;
        }
        let Some(memo) = n.decided_memo.as_ref().and_then(|m| <[u8; 512]>::try_from(m.as_slice()).ok()) else { continue };
        let Ok(m) = DepositMemo::decode(&memo) else { continue };
        if rt.secrets.payout_commitment(&m.payout_address, &m.session_key, app) == *commitment {
            return Ok(Some(m.payout_address));
        }
    }
    Ok(None)
}

fn action_target(sizes: &[u32], needed: u32) -> u32 {
    sizes.iter().copied().filter(|s| *s >= needed).min().unwrap_or(needed)
}

/// This operator's plan for `app`'s next head transition, or why it waits.
pub fn plan(node: &Node, ledger: &dyn Ledger, app: &AppId, now: i64) -> Result<std::result::Result<Planned, Wait>> {
    if let Some(r) = node.halted()? {
        return Err(NodeError::Halted(r));
    }
    let rt = &node.apps[app];
    let pol = &node.cfg.payout;
    let Some(head) = node.head(app) else { return Ok(Err(Wait::NoHead)) };
    let Some(current) = head.chain.last() else { return Ok(Err(Wait::NoHead)) };
    if let Some(p) = head.pending {
        return Ok(Err(Wait::Pending(p)));
    }
    // A transition this cluster broadcast that is neither mined nor expired.
    if let Some(v) = node.store.meta(&meta_key("pending", app))? {
        if v.len() == 36 {
            let txid: [u8; 32] = v[..32].try_into().unwrap();
            let expiry = u32::from_le_bytes(v[32..].try_into().unwrap());
            let tip = node.store.tip()?.unwrap_or(0);
            if head.chain.iter().any(|l| l.txid == txid) || tip >= expiry {
                node.store.del_meta(&meta_key("pending", app))?;
            } else {
                return Ok(Err(Wait::Pending(txid)));
            }
        }
    }
    let Some(acc) = ledger.app(&rt.program)? else { return Ok(Err(Wait::AppNotActive)) };
    let cfg = ledger.config()?;
    if acc.state != AppState::Active || cfg.paused {
        return Ok(Err(Wait::AppNotActive));
    }
    // Rule 4, as of this finalized read.
    let tip = node.store.tip()?.unwrap_or(0);
    let notes = node.store.notes(app)?;
    let deposits = ledger.deposits(&rt.program)?;
    let ctx = ReconcileCtx {
        network: node.cfg.network(),
        vault_address: rt.vault_address,
        secrets: &rt.secrets,
        tip,
        depths: &node.cfg.rule(),
    };
    let intents = |_idx: u128| None;
    if !reconcile(&acc, &deposits, &notes, &intents, &ctx, Some(&head)).is_empty() {
        return Ok(Err(Wait::Unreconciled));
    }

    // Rule 3: the records that follow the head's paid count, in order.
    let mut settlements: BTreeMap<u64, _> = ledger.settlements(&rt.program)?.into_iter().map(|s| (s.sequence, s)).collect();
    let mut records = Vec::new();
    let mut total: u64 = 0;
    let mut needs_all = false;
    let mut seq = current.paid_count;
    while (records.len() as u32) < pol.max_records_per_tx {
        let Some(s) = settlements.remove(&seq) else { break };
        let mut lines = Vec::new();
        for p in &s.payouts[..s.count as usize] {
            let Some(to) = resolve_address(node, app, &p.commitment)? else {
                return Ok(Err(Wait::Unresolved { sequence: seq, commitment: p.commitment }));
            };
            if p.amount > pol.payout_cap {
                // Large lines wait out a delay from when this operator first saw them.
                let k = format!("first_seen:{}:{seq}", hex::encode(app));
                let first = match meta_i64(node, &k)? {
                    Some(t) => t,
                    None => {
                        node.store.set_meta(&k, &now.to_le_bytes())?;
                        now
                    }
                };
                if now - first < pol.large_delay_secs {
                    return Ok(Err(Wait::LargeDelay { sequence: seq }));
                }
                needs_all = true;
            }
            total = total.saturating_add(p.amount);
            lines.push(PayoutLine { commitment: p.commitment, amount: p.amount, to: to.to_vec() });
        }
        records.push(PayoutRecord { sequence: seq, payouts: lines });
        seq += 1;
    }

    // Credited deposit notes on the finalized ledger that this scan also holds.
    let credited_ids: std::collections::HashSet<[u8; 32]> =
        deposits.iter().filter(|(_, d)| d.state == DepositState::Credited).map(|(id, _)| *id).collect();
    let credited: Vec<[u8; 32]> = notes
        .iter()
        .filter(|n| n.kind == NoteKind::Deposit && !n.rolled_back && n.spent.is_none() && credited_ids.contains(&n.deposit_id))
        .map(|n| n.note.cmx)
        .collect();

    let last_payout = meta_i64(node, &meta_key("last_payout", app))?.unwrap_or(i64::MIN / 2);
    if records.is_empty() {
        // A transition that pays nothing: only to consolidate, and not too often.
        let last_c = meta_i64(node, &meta_key("last_consolidation", app))?.unwrap_or(i64::MIN / 2);
        if (credited.len() as u32) < node.cfg.payout.consolidate_at_notes || now - last_c < pol.consolidate_interval_secs {
            return Ok(Err(Wait::Nothing));
        }
    } else if now - last_payout < pol.interval_secs && (records.len() as u32) < pol.batch_records {
        return Ok(Err(Wait::Schedule));
    }

    // Rolling hourly cap.
    let window_key = meta_key("hour_window", app);
    let (window_start, window_sum) = match node.store.meta(&window_key)? {
        Some(v) if v.len() == 16 => (i64::from_le_bytes(v[..8].try_into().unwrap()), u64::from_le_bytes(v[8..].try_into().unwrap())),
        _ => (now, 0),
    };
    let window_sum = if now - window_start >= 3_600 { 0 } else { window_sum };
    if window_sum.saturating_add(total) > pol.hourly_cap {
        return Ok(Err(Wait::HourlyCap));
    }

    // Claims left after this transition: liability plus settlements that are
    // unpaid by the head chain and not paid here.
    let paying: std::collections::HashSet<u64> = records.iter().map(|r| r.sequence).collect();
    let unpaid_after: u64 = ledger
        .settlements(&rt.program)?
        .iter()
        .filter(|s| s.state == SettlementState::Unpaid && s.sequence >= current.paid_count && !paying.contains(&s.sequence))
        .map(|s| s.total)
        .sum();

    let outputs = records.iter().map(|r| r.payouts.len() as u32).sum::<u32>() + 1;
    let target = action_target(&pol.action_sizes, outputs.max(2));
    // ZIP 317: 5,000 zat per logical action, at least 2. Inputs are capped at
    // target − 1 besides the head, so the transition has exactly `target` actions.
    let need = 5_000 * target.max(2) as u64;
    let have = (acc.fee_pool as u128 + current.fee_topups_swept as u128).saturating_sub(current.network_fees_spent as u128) as u64;
    if need > have {
        return Ok(Err(Wait::FeePool { need, have }));
    }
    let plan = PayoutPlan {
        app: *app,
        genesis_head: acc.genesis_head,
        records,
        credited,
        fee_pool_accrued: acc.fee_pool,
        claims_after: acc.liability.saturating_add(unpaid_after),
        action_target: target,
        max_inputs: pol.max_inputs.min(target.saturating_sub(1)),
        // zol-zcashd counts plain blocks.
        full_depth: node.cfg.rule().blocks(node.cfg.depths.full, tip),
        head_depth: 1,
        expiry_delta: pol.expiry_delta,
    };
    Ok(Ok(Planned { plan, needs_all, total }))
}

/// Records a broadcast head transition: pending until mined or expired, and
/// counted for scheduling and the hourly cap.
pub fn record_broadcast(node: &Node, app: &AppId, planned: &Planned, txid: &[u8; 32], expiry: u32, now: i64) -> Result<()> {
    let mut p = txid.to_vec();
    p.extend_from_slice(&expiry.to_le_bytes());
    node.store.set_meta(&meta_key("pending", app), &p)?;
    let k = if planned.plan.records.is_empty() { "last_consolidation" } else { "last_payout" };
    node.store.set_meta(&meta_key(k, app), &now.to_le_bytes())?;
    let window_key = meta_key("hour_window", app);
    let (start, sum) = match node.store.meta(&window_key)? {
        Some(v) if v.len() == 16 => (i64::from_le_bytes(v[..8].try_into().unwrap()), u64::from_le_bytes(v[8..].try_into().unwrap())),
        _ => (now, 0),
    };
    let (start, sum) = if now - start >= 3_600 { (now, 0) } else { (start, sum) };
    let mut v = start.to_le_bytes().to_vec();
    v.extend_from_slice(&sum.saturating_add(planned.total).to_le_bytes());
    node.store.set_meta(&window_key, &v)?;
    Ok(())
}

/// Helper for tests and tools: a head chain that is only the genesis head.
pub fn genesis_only(app: AppId, cmx: [u8; 32], value: u64, height: u32) -> HeadChain {
    HeadChain {
        app,
        chain: vec![zol_types::api::HeadLink {
            cmx,
            value,
            position: 0,
            paid_count: 0,
            records_digest: [0; 32],
            network_fees_spent: 0,
            fee_topups_swept: 0,
            txid: [0; 32],
            height,
        }],
        pending: None,
        fault: None,
        tip: height,
    }
}
