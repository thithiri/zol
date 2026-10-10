//! Reconciliation (plan 3.7; design signer rule 4) and the reserve computed
//! from one operator's own scan.
//!
//! As of a finalized ledger read, every credited `Deposit` of the app, index 0
//! to deposit_count − 1, must match a note in this operator's scan: same value,
//! and an attestation digest this operator recomputes from that note and its
//! memo. The credited total must equal their sum, and credits not yet at full
//! depth must fit in the remaining float. Any mismatch freezes the app.

use std::collections::HashMap;

use zol_ledger::state::{App, Deposit, DepositState};
use zol_wire::{HeadMemo, Network, ORCHARD_ADDR_LEN};

use crate::chain::Scope;
use crate::config::{AppSecrets, Depths};
use crate::decide::{recompute_credit_digest, resolve_memo};
use crate::policy::{depth, Depth, Reserve};
use crate::store::{NoteKind, NoteRecord};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// `App.deposit_count` differs from the number of credited deposits, or an index is missing or repeated.
    Indexing { deposit_count: u64, credited: usize },
    /// A credited deposit has no note behind it in this scan: a forged credit.
    NoNote { deposit_id: [u8; 32] },
    AmountDiffers { deposit_id: [u8; 32], ledger: u64, note: u64 },
    /// The stored digest is not what this note and memo give: a misbound credit.
    DigestDiffers { deposit_id: [u8; 32] },
    CreditedTotal { ledger: u64, notes: u128 },
    /// Credits short of full depth exceed the remaining float.
    EarlyOverFloat { pending: u128, float: u128 },
}

pub struct ReconcileCtx<'a> {
    pub network: Network,
    pub vault_address: [u8; ORCHARD_ADDR_LEN],
    pub secrets: &'a AppSecrets,
    pub tip: u32,
    pub depths: &'a Depths,
}

/// Value of credits not yet at full depth (rolled-back ones count as lost, not pending).
pub fn pending_early(notes: &[NoteRecord], tip: u32, depths: &Depths) -> u128 {
    notes
        .iter()
        .filter(|n| n.kind == NoteKind::Deposit && n.state == crate::store::NoteState::Credited && !n.rolled_back)
        .filter(|n| depth(n.note.height, tip, depths) < Depth::Full)
        .map(|n| n.note.value as u128)
        .sum()
}

/// The current head: the unspent, non-rolled-back head note in the internal scope.
/// (Phase 4 replaces this with a walk of the nullifier chain from the genesis head.)
pub fn current_head(notes: &[NoteRecord]) -> Option<(&NoteRecord, HeadMemo)> {
    notes
        .iter()
        .filter(|n| n.kind == NoteKind::Head && n.note.scope == Scope::Internal && !n.rolled_back && n.spent.is_none())
        .filter_map(|n| HeadMemo::decode(&crate::chain::memo_array(&n.note)).ok().map(|m| (n, m)))
        .max_by_key(|(_, m)| m.position)
}

/// Reserve position from this operator's scan and the finalized ledger.
pub fn compute_reserve(app: &App, notes: &[NoteRecord], tip: u32, depths: &Depths) -> Reserve {
    let live = |n: &&NoteRecord| !n.rolled_back && n.spent.is_none();
    let head = current_head(notes);
    let head_value = head.map(|(n, _)| n.note.value as u128).unwrap_or(0);
    let credited: u128 = notes
        .iter()
        .filter(live)
        .filter(|n| n.kind == NoteKind::Deposit && n.state == crate::store::NoteState::Credited)
        .map(|n| n.note.value as u128)
        .sum();
    let topups: u128 = notes
        .iter()
        .filter(live)
        .filter(|n| n.kind == NoteKind::TopUp && depth(n.note.height, tip, depths) == Depth::Full)
        .map(|n| n.note.value as u128)
        .sum();
    let (swept, spent) = head.map(|(_, m)| (m.fee_topups_swept as u128, m.network_fees_spent as u128)).unwrap_or((0, 0));
    Reserve {
        reserve: head_value + credited + topups,
        claims: app.liability as u128 + app.unpaid_total as u128,
        fee_pool_left: (app.fee_pool as u128 + swept).saturating_sub(spent),
    }
}

pub fn reconcile(
    app: &App,
    deposits: &[([u8; 32], Deposit)],
    notes: &[NoteRecord],
    intents: &dyn Fn(u128) -> Option<[u8; 512]>,
    ctx: &ReconcileCtx,
) -> Vec<Mismatch> {
    let mut out = Vec::new();
    let credited: Vec<&([u8; 32], Deposit)> = deposits.iter().filter(|(_, d)| d.state == DepositState::Credited).collect();
    let mut seen = vec![false; app.deposit_count as usize];
    let mut indexing_ok = credited.len() as u64 == app.deposit_count;
    for (_, d) in &credited {
        match seen.get_mut(d.index as usize) {
            Some(s) if !*s => *s = true,
            _ => indexing_ok = false,
        }
    }
    if !indexing_ok {
        out.push(Mismatch::Indexing { deposit_count: app.deposit_count, credited: credited.len() });
    }

    let by_id: HashMap<[u8; 32], &NoteRecord> =
        notes.iter().filter(|n| n.kind == NoteKind::Deposit).map(|n| (n.deposit_id, n)).collect();
    let mut total: u128 = 0;
    for (id, d) in &credited {
        let Some(n) = by_id.get(id) else {
            out.push(Mismatch::NoNote { deposit_id: *id });
            continue;
        };
        // The credited total must equal the sum of the real notes behind it.
        total += n.note.value as u128;
        if n.note.value != d.amount {
            out.push(Mismatch::AmountDiffers { deposit_id: *id, ledger: d.amount, note: n.note.value });
            continue;
        }
        // The memo this note was decided from, else what the scan resolves now.
        let memo = n.decided_memo.as_ref().and_then(|m| <[u8; 512]>::try_from(m.as_slice()).ok()).or_else(|| {
            resolve_memo(&n.note, intents(n.note.receiver_index).as_ref()).map(|(_, canon, _)| canon)
        });
        let digest = memo.and_then(|m| recompute_credit_digest(&n.note, &m, ctx.network, &ctx.vault_address, ctx.secrets));
        if digest != Some(d.digest) {
            out.push(Mismatch::DigestDiffers { deposit_id: *id });
        }
    }
    if total != app.credited_total as u128 {
        out.push(Mismatch::CreditedTotal { ledger: app.credited_total, notes: total });
    }
    let reserve = compute_reserve(app, notes, ctx.tip, ctx.depths);
    let pending = pending_early(notes, ctx.tip, ctx.depths);
    if pending > reserve.remaining_float() {
        out.push(Mismatch::EarlyOverFloat { pending, float: reserve.remaining_float() });
    }
    out
}
