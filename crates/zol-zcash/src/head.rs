//! The vault head chain (design v0.3.1, "The vault head"), walked by nullifiers from the
//! genesis commitment pinned on Solana, over this operator's own scan.
//!
//! - Genesis: the pinned `cmx` must be a scanned note of this app, internal scope, whose
//!   memo is `HeadMemo::genesis(network, app)`.
//! - The transaction that reveals the current head's nullifier is the next transition; its
//!   **only** output addressed to the vault (any diversifier, both scopes) is the new head.
//! - The new head's memo must equal `prev.advance(Transition { records_paid, records_digest,
//!   network_fee, fee_topups_swept })` where `network_fee` is the transaction's Ironwood
//!   value balance (the fee of a pure-Ironwood transaction) and `fee_topups_swept` is the
//!   value of this app's fee-pool top-up notes the transaction spends.
//! - Anything else sets `fault` and stops the walk: signers must halt.
//!
//! Notes that merely look like heads are never reached: only nullifiers link the chain.
//! Reorgs are followed because the scanner rewinds notes and spends.

use orchard::keys::Scope;
use zol_types::api::HeadLink;
use zol_wire::{HeadMemo, TopUpMemo, TopUpPurpose, Transition};

use crate::{keys::AppKeys, scan::Pool, scan::ScannedNote, Scanner};

#[derive(Clone, Debug)]
pub struct Link {
    pub note: ScannedNote,
    pub memo: HeadMemo,
}

impl Link {
    pub fn to_api(&self) -> HeadLink {
        HeadLink {
            cmx: self.note.cmx,
            value: self.note.value,
            position: self.memo.position,
            paid_count: self.memo.paid_count,
            records_digest: self.memo.records_digest,
            network_fees_spent: self.memo.network_fees_spent,
            fee_topups_swept: self.memo.fee_topups_swept,
            txid: self.note.txid,
            height: self.note.height,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HeadWalk {
    pub links: Vec<Link>,
    pub fault: Option<String>,
}

impl HeadWalk {
    /// The current (unspent) head, if the walk is sound.
    pub fn current(&self) -> Option<&Link> {
        if self.fault.is_some() {
            None
        } else {
            self.links.last()
        }
    }
}

/// A fee-pool top-up of this app (by memo).
pub fn is_fee_topup(n: &ScannedNote, keys: &AppKeys) -> bool {
    matches!(TopUpMemo::decode(&n.memo), Ok(t) if t.app == keys.app_id && t.network == keys.network && t.purpose == TopUpPurpose::FeePool)
}

/// Any top-up of this app (by memo).
pub fn is_topup(n: &ScannedNote, keys: &AppKeys) -> bool {
    matches!(TopUpMemo::decode(&n.memo), Ok(t) if t.app == keys.app_id && t.network == keys.network)
}

pub fn walk(scanner: &Scanner, keys: &AppKeys, genesis: &[u8; 32]) -> HeadWalk {
    let mut w = HeadWalk::default();
    let fault = |w: &mut HeadWalk, s: String| {
        w.fault = Some(s);
    };
    let Some(g) = scanner
        .note_by_cmx(genesis)
        .filter(|n| n.app_id == keys.app_id && n.pool == Pool::Ironwood)
    else {
        fault(&mut w, "genesis_not_found".into());
        return w;
    };
    if g.scope != Scope::Internal {
        fault(&mut w, "genesis_not_internal".into());
        return w;
    }
    let gm = match HeadMemo::decode(&g.memo) {
        Ok(m) if m == HeadMemo::genesis(keys.network, keys.app_id) => m,
        _ => {
            fault(&mut w, "genesis_memo".into());
            return w;
        }
    };
    w.links.push(Link {
        note: g.clone(),
        memo: gm,
    });
    loop {
        let cur = w.links.last().unwrap().clone();
        let Some(spend) = scanner.spend_of(&cur.note.cmx) else {
            return w; // current head is unspent
        };
        let txid = spend.spending_txid;
        let outs: Vec<&ScannedNote> = scanner
            .notes_in_tx(&txid)
            .into_iter()
            .filter(|n| n.app_id == keys.app_id)
            .collect();
        let at = format!(
            "transition {} (tx {})",
            cur.memo.position + 1,
            crate::display_hex(&txid)
        );
        if outs.is_empty() {
            fault(&mut w, format!("head_spent_without_successor: {at}"));
            return w;
        }
        if outs.len() > 1 {
            fault(&mut w, format!("two_vault_outputs: {at}"));
            return w;
        }
        let n = outs[0];
        if n.pool != Pool::Ironwood {
            fault(&mut w, format!("head_not_ironwood: {at}"));
            return w;
        }
        let memo = match HeadMemo::decode(&n.memo) {
            Ok(m) => m,
            Err(_) => {
                fault(&mut w, format!("head_memo_invalid: {at}"));
                return w;
            }
        };
        let rec = match scanner.tx_record(&txid) {
            Some(r)
                if !r.has_transparent
                    && !r.has_sapling
                    && r.orchard_value_balance == 0
                    && r.ironwood_value_balance >= 0 =>
            {
                *r
            }
            _ => {
                fault(&mut w, format!("transition_not_pure_ironwood: {at}"));
                return w;
            }
        };
        let swept: u64 = scanner
            .spends_in_tx(&txid)
            .iter()
            .filter_map(|s| scanner.note_by_cmx(&s.cmx))
            .filter(|x| x.app_id == keys.app_id && is_fee_topup(x, keys))
            .map(|x| x.value)
            .sum();
        let records_paid = memo.paid_count.wrapping_sub(cur.memo.paid_count);
        let expected = (memo.paid_count >= cur.memo.paid_count)
            .then(|| {
                cur.memo.advance(&Transition {
                    records_paid,
                    records_digest: memo.records_digest,
                    network_fee: rec.ironwood_value_balance as u64,
                    fee_topups_swept: swept,
                })
            })
            .flatten();
        if expected != Some(memo) {
            fault(&mut w, format!("head_memo_advance: {at}"));
            return w;
        }
        w.links.push(Link {
            note: n.clone(),
            memo,
        });
    }
}
