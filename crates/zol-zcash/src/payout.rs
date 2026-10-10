//! Head transitions: settlement payouts and consolidations (design v0.3.1, "Exit: settlement
//! and payout", "Reserve accounting").
//!
//! [`propose`] builds the coordinator's proposal; [`verify`] is what every operator runs
//! against its **own** plan and **own** scan before committing a FROST nonce. Each failed
//! check is reported by a stable name (see [`FAILURES`]).

use std::collections::BTreeMap;

use ff::PrimeField;
use orchard::{
    keys::{FullViewingKey, Scope},
    Address,
};
use pczt::Pczt;
use zol_types::api::{PayoutPlan, Proposal, VerifyResponse};
use zol_wire::{
    digest::{record_digest, records_digest, Payout, Sha256},
    HeadMemo, Transition,
};

use crate::{
    checks,
    head::{self, is_fee_topup, is_topup, Link},
    keys::{AppKeys, Receiver},
    params::ChainParams,
    refund::throwaway_receiver,
    scan::{Pool, ScannedNote, Spendable},
    tx::{self, OutputSpec, SpendSpec},
    Error, Scanner, MAX_EXPIRY_DELTA, ZIP317_GRACE_ACTIONS, ZIP317_MARGINAL_FEE,
};

/// Anchors accepted from the last this-many blocks: 300 blocks is about 2 h at NU7's
/// 25 s spacing (was 100 at 75 s).
pub const ANCHOR_WINDOW: u32 = 300;

/// Every failure name [`verify`] can report.
pub const FAILURES: &[&str] = &[
    "pczt_parse",
    "sighash",
    "head_fault",
    "head_pending",
    "head_depth",
    "plan_app_mismatch",
    "records_not_next",
    "payout_receiver_invalid",
    "dummy_key_present",
    "redacted_field",
    "user_address",
    "anchor_unknown",
    "head_not_spent",
    "real_spend_cv",
    "spend_not_allowed",
    "real_spend_unknown_note",
    "duplicate_spend",
    "too_many_inputs",
    "dummy_nonzero",
    "dummy_signature",
    "cmx_mismatch",
    "vault_outputs_count",
    "head_scope",
    "head_value",
    "head_memo_advance",
    "payout_missing",
    "unexpected_value_output",
    "payout_memo",
    "padding_count",
    "action_count",
    "epk",
    "enc_ciphertext",
    "out_ciphertext",
    "fee_shape",
    "fee_pool",
    "value_balance",
    "expiry",
    "branch",
    "proof",
    "reserve",
];

pub fn zip317_fee(actions: u64) -> u64 {
    ZIP317_MARGINAL_FEE * actions.max(ZIP317_GRACE_ACTIONS)
}

/// The payout lines of a plan's records, in order, and the records digest.
pub struct PlannedRecords {
    pub lines: Vec<(Receiver, u64)>,
    pub records_paid: u64,
    pub records_digest: [u8; 32],
}

/// Checks the plan's records follow `head` (consecutive from its `paid_count`) and resolves
/// them. Errors carry a failure name.
pub fn plan_records(plan: &PayoutPlan, head: &HeadMemo) -> Result<PlannedRecords, &'static str> {
    let mut lines = vec![];
    let mut digests = vec![];
    for (i, r) in plan.records.iter().enumerate() {
        if r.sequence != head.paid_count + i as u64 {
            return Err("records_not_next");
        }
        let mut ps = vec![];
        for l in &r.payouts {
            let to: Receiver =
                l.to.clone()
                    .try_into()
                    .map_err(|_| "payout_receiver_invalid")?;
            if bool::from(Address::from_raw_address_bytes(&to).is_none()) || l.amount == 0 {
                return Err("payout_receiver_invalid");
            }
            lines.push((to, l.amount));
            ps.push(Payout {
                commitment: l.commitment,
                amount: l.amount,
            });
        }
        digests.push(record_digest::<Sha256>(&plan.app, r.sequence, &ps));
    }
    let records_digest = if digests.is_empty() {
        [0; 32]
    } else {
        records_digest::<Sha256>(&plan.app, head.paid_count, &digests)
    };
    Ok(PlannedRecords {
        lines,
        records_paid: plan.records.len() as u64,
        records_digest,
    })
}

fn confirmations(tip: u32, height: u32) -> u32 {
    (tip + 1).saturating_sub(height)
}

/// Notes besides the head a transition may spend: unspent credited deposit notes (`cmx` in
/// `plan.credited`) and unspent top-ups at `full_depth`. Keyed by nullifier.
pub fn eligible(
    scanner: &Scanner,
    keys: &AppKeys,
    plan: &PayoutPlan,
    head_cmx: &[u8; 32],
) -> BTreeMap<[u8; 32], ScannedNote> {
    let tip = scanner.tip().0;
    scanner
        .unspent(&keys.app_id)
        .filter(|n| n.pool == Pool::Ironwood && &n.cmx != head_cmx)
        .filter(|n| {
            plan.credited.contains(&n.cmx)
                || (is_topup(n, keys) && confirmations(tip, n.height) >= plan.full_depth)
        })
        .map(|n| (n.nullifier, n.clone()))
        .collect()
}

/// The fee pool left before this transition: accrued + fee top-ups swept − network fees spent.
pub fn fee_pool_left(plan: &PayoutPlan, head: &HeadMemo) -> u64 {
    (plan.fee_pool_accrued + head.fee_topups_swept).saturating_sub(head.network_fees_spent)
}

/// A dishonest coordinator's knobs, for adversarial tests only.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct Tamper {
    pub extra_payout: Option<(Receiver, u64)>,
    pub drop_first_payout: bool,
    pub first_amount_delta: i64,
    pub first_receiver: Option<Receiver>,
    /// Extra spends (owner FVK, note) added beyond the honest selection.
    pub extra_inputs: Vec<(FullViewingKey, Spendable)>,
    pub extra_vault_output: bool,
    pub memo_override: Option<HeadMemo>,
    pub skip_io_finalize: bool,
    /// Spend this head instead of the walked one (e.g. another app's), with its FVK.
    pub head_override: Option<(FullViewingKey, Spendable, HeadMemo)>,
}

/// The coordinator's proposal for `plan` (honest when `tamper` is default).
pub fn propose(
    params: &ChainParams,
    keys: &AppKeys,
    scanner: &Scanner,
    plan: &PayoutPlan,
    tamper: &Tamper,
) -> Result<(Proposal, Pczt), Error> {
    let e = |s: &str| Error::Build(s.to_string());
    if plan.app != keys.app_id {
        return Err(e("plan is for another app"));
    }
    let tip = scanner.tip().0;
    let walk = head::walk(scanner, keys, &plan.genesis_head);
    if let Some(f) = &walk.fault {
        return Err(Error::Build(format!("head chain fault: {f}")));
    }
    let head: Link = walk.current().cloned().ok_or_else(|| e("no head"))?;
    if confirmations(tip, head.note.height) < plan.head_depth {
        return Err(e("head below head_depth"));
    }
    let pr = plan_records(plan, &head.memo).map_err(e)?;
    let mut lines = pr.lines.clone();
    if tamper.drop_first_payout && !lines.is_empty() {
        lines.remove(0);
    }
    if let Some(first) = lines.first_mut() {
        first.1 = (first.1 as i64 + tamper.first_amount_delta) as u64;
        if let Some(r) = tamper.first_receiver {
            first.0 = r;
        }
    }
    if let Some(x) = tamper.extra_payout {
        lines.push(x);
    }
    let total_pay: u64 = lines.iter().map(|l| l.1).sum();
    let pool_base = fee_pool_left(plan, &head.memo);
    let a = plan.action_target as u64;
    let n_out_fixed = lines.len() as u64 + 1 + tamper.extra_vault_output as u64;
    let shape = |n_spends: u64| -> (u64, u64) {
        let actions = a.max(n_spends).max(n_out_fixed);
        (actions - n_out_fixed, zip317_fee(actions)) // (padding outputs, fee)
    };

    // Inputs: the head, then credited deposits (oldest first), float top-ups, fee top-ups.
    let mut cands: Vec<ScannedNote> = eligible(scanner, keys, plan, &head.note.cmx)
        .into_values()
        .collect();
    cands.sort_by_key(|n| {
        (
            is_topup(n, keys) as u8 + is_fee_topup(n, keys) as u8,
            n.height,
            n.action_index,
        )
    });
    let consolidation = plan.records.is_empty();
    let (head_fvk, head_sp, head_memo) = match &tamper.head_override {
        Some((f, sp, m)) => (f.clone(), sp.clone(), *m),
        None => (
            keys.fvk().clone(),
            scanner.spendable(&head.note.cmx, Some(tip))?,
            head.memo,
        ),
    };
    let mut extra: Vec<ScannedNote> = vec![];
    let extra_tamper_value: u64 = tamper
        .extra_inputs
        .iter()
        .map(|(_, sp)| sp.note.value().inner())
        .sum();
    let ok = |extra: &[ScannedNote]| {
        let n = 1 + extra.len() as u64 + tamper.extra_inputs.len() as u64;
        let (_, fee) = shape(n);
        let swept: u64 = extra
            .iter()
            .filter(|x| is_fee_topup(x, keys))
            .map(|x| x.value)
            .sum();
        let value_in = head_sp.note.value().inner()
            + extra.iter().map(|x| x.value).sum::<u64>()
            + extra_tamper_value;
        value_in >= total_pay + fee && fee <= pool_base + swept
    };
    for c in cands {
        if extra.len() as u32 >= plan.max_inputs {
            break;
        }
        if !consolidation && ok(&extra) {
            break;
        }
        extra.push(c);
    }
    if !ok(&extra) {
        return Err(e(
            "inputs within max_inputs do not cover payouts + fee, or the fee exceeds the fee pool",
        ));
    }
    let n_spends = 1 + extra.len() as u64 + tamper.extra_inputs.len() as u64;
    let (pad, fee) = shape(n_spends);
    let swept: u64 = extra
        .iter()
        .filter(|x| is_fee_topup(x, keys))
        .map(|x| x.value)
        .sum();
    let value_in = head_sp.note.value().inner()
        + extra.iter().map(|x| x.value).sum::<u64>()
        + extra_tamper_value;
    let new_head_value = value_in - total_pay - fee;
    let memo = tamper.memo_override.unwrap_or(
        head_memo
            .advance(&Transition {
                records_paid: pr.records_paid,
                records_digest: pr.records_digest,
                network_fee: fee,
                fee_topups_swept: swept,
            })
            .ok_or_else(|| e("head memo overflow"))?,
    );

    let empty = tx::empty_memo();
    let mut outputs: Vec<OutputSpec> = lines
        .iter()
        .map(|(to, v)| OutputSpec {
            to: *to,
            value: *v,
            ovk: Some(keys.ovk(Scope::External)),
            memo: empty,
        })
        .collect();
    outputs.push(OutputSpec {
        to: keys.vault_internal_address(),
        value: new_head_value,
        ovk: Some(keys.ovk(Scope::Internal)),
        memo: memo.encode(),
    });
    if tamper.extra_vault_output {
        outputs.push(OutputSpec {
            to: keys.vault_address(),
            value: 0,
            ovk: Some(keys.padding_ovk()),
            memo: empty,
        });
    }
    for _ in 0..pad {
        outputs.push(OutputSpec {
            to: throwaway_receiver(&mut rand::rngs::OsRng),
            value: 0,
            ovk: Some(keys.padding_ovk()),
            memo: empty,
        });
    }
    let mut spends = vec![SpendSpec {
        fvk: head_fvk,
        note: head_sp.note,
        path: head_sp.path.clone(),
    }];
    let mut inputs = vec![head_sp.scanned.cmx];
    for x in &extra {
        let sp = scanner.spendable(&x.cmx, Some(tip))?;
        spends.push(SpendSpec {
            fvk: keys.fvk().clone(),
            note: sp.note,
            path: sp.path,
        });
        inputs.push(x.cmx);
    }
    for (f, sp) in &tamper.extra_inputs {
        spends.push(SpendSpec {
            fvk: f.clone(),
            note: sp.note,
            path: sp.path.clone(),
        });
        inputs.push(sp.scanned.cmx);
    }
    let built = tx::build_ironwood(
        params,
        tip + 1,
        plan.expiry_delta,
        &spends,
        head_sp.anchor,
        &outputs,
        None,
    )?;
    if built.fee != fee {
        return Err(Error::Build(format!(
            "builder fee {} != planned {fee}",
            built.fee
        )));
    }
    let pczt = if tamper.skip_io_finalize {
        tx::prove(built.pczt)?
    } else {
        tx::prove(tx::io_finalize(built.pczt)?)?
    };
    let sighash = tx::sighash(&pczt)?;
    let bytes = pczt
        .clone()
        .serialize()
        .map_err(|e| Error::Pczt(format!("{e:?}")))?;
    Ok((
        Proposal {
            app: keys.app_id,
            pczt: bytes,
            sighash,
            expiry_height: built.expiry_height,
            inputs,
            fee,
            new_head_value,
        },
        pczt,
    ))
}

fn ciphertext_failure(e: &Error) -> &'static str {
    let s = e.to_string();
    if s.contains("epk") {
        "epk"
    } else if s.contains("enc_ciphertext") {
        "enc_ciphertext"
    } else if s.contains("out_ciphertext") {
        "out_ciphertext"
    } else {
        "redacted_field"
    }
}

/// Verifies `pczt` against this operator's own `plan` and own `scanner`. `pending` is a
/// broadcast, unmined transition (its txid = sighash) this operator knows for the head.
pub fn verify(
    keys: &AppKeys,
    scanner: &Scanner,
    plan: &PayoutPlan,
    pczt_bytes: &[u8],
    pending: Option<[u8; 32]>,
) -> VerifyResponse {
    let mut f: Vec<String> = vec![];
    let fail = |f: &mut Vec<String>, s: &str| {
        if !f.iter().any(|x| x == s) {
            f.push(s.to_string());
        }
    };
    let done = |f: Vec<String>, sighash: [u8; 32]| VerifyResponse {
        ok: f.is_empty(),
        failures: f,
        sighash,
    };
    let Ok(pczt) = Pczt::parse(pczt_bytes) else {
        fail(&mut f, "pczt_parse");
        return done(f, [0; 32]);
    };
    let sighash = match tx::sighash(&pczt) {
        Ok(s) => s,
        Err(_) => {
            fail(&mut f, "sighash");
            return done(f, [0; 32]);
        }
    };
    if plan.app != keys.app_id {
        fail(&mut f, "plan_app_mismatch");
        return done(f, sighash);
    }
    if pending.is_some_and(|p| p != sighash) {
        fail(&mut f, "head_pending");
    }
    let tip = scanner.tip().0;
    let walk = head::walk(scanner, keys, &plan.genesis_head);
    let Some(head) = walk.current().cloned() else {
        fail(&mut f, "head_fault");
        return done(f, sighash);
    };
    if confirmations(tip, head.note.height) < plan.head_depth {
        fail(&mut f, "head_depth");
    }
    let pr = match plan_records(plan, &head.memo) {
        Ok(p) => p,
        Err(name) => {
            fail(&mut f, name);
            return done(f, sighash);
        }
    };
    let exp = *pczt.global().expiry_height();
    if plan.expiry_delta > MAX_EXPIRY_DELTA || exp <= tip || exp > tip + 1 + plan.expiry_delta {
        fail(&mut f, "expiry");
    }
    // The branch the next block will use, and no expiry past the next upgrade (NU7): the
    // signatures commit to the branch ID, so anything else can never be mined.
    let params = scanner.config().params;
    if *pczt.global().consensus_branch_id() != u32::from(params.branch_id(tip + 1))
        || exp > tx::capped_expiry(&params, tip + 1, MAX_EXPIRY_DELTA)
    {
        fail(&mut f, "branch");
    }
    let elig = eligible(scanner, keys, plan, &head.note.cmx);
    let fvk = keys.fvk();
    let anchors: Vec<_> = (tip.saturating_sub(ANCHOR_WINDOW)..=tip)
        .filter_map(|h| scanner.anchor_at(h).ok().flatten())
        .collect();
    let r = tx::with_ironwood(&pczt, |b| {
        let mut f2: Vec<String> = vec![];
        let fl = &mut f2;
        let push = |fl: &mut Vec<String>, s: &str| {
            if !fl.iter().any(|x| x == s) {
                fl.push(s.to_string());
            }
        };
        if !anchors.contains(b.anchor()) {
            push(fl, "anchor_unknown");
        }
        // Completeness.
        let mut complete = true;
        for a in b.actions() {
            if a.spend().dummy_sk().is_some() {
                push(fl, "dummy_key_present");
            }
            if a.output().user_address().is_some() {
                push(fl, "user_address");
            }
            if a.rcv().is_none() || a.spend().alpha().is_none() || checks::output_note(a).is_err() {
                push(fl, "redacted_field");
                complete = false;
            }
        }
        if !complete {
            return (f2, 0u64);
        }
        // Spends.
        let mut head_spent = false;
        let mut seen = std::collections::BTreeSet::new();
        let mut value_in = 0u64;
        let mut real = 0u64;
        let mut swept = 0u64;
        let mut spent_elig = std::collections::BTreeSet::new();
        for a in b.actions() {
            let nf = a.spend().nullifier().to_bytes();
            if checks::rk_matches(a, fvk) {
                real += 1;
                if !seen.insert(nf) {
                    push(fl, "duplicate_spend");
                }
                let note = if nf == head.note.nullifier {
                    head_spent = true;
                    Some(head.note.clone())
                } else if let Some(n) = elig.get(&nf) {
                    spent_elig.insert(nf);
                    if is_fee_topup(n, keys) {
                        swept += n.value;
                    }
                    Some(n.clone())
                } else if scanner
                    .all_notes()
                    .any(|n| n.nullifier == nf && n.app_id == keys.app_id)
                {
                    push(fl, "spend_not_allowed");
                    None
                } else {
                    push(fl, "real_spend_unknown_note");
                    None
                };
                if let Some(n) = note {
                    value_in += n.value;
                    if checks::check_cv_net(a, n.value).is_err() {
                        push(fl, "real_spend_cv");
                    }
                }
            } else {
                if checks::check_cv_net(a, 0).is_err() {
                    push(fl, "dummy_nonzero");
                }
                let sig_ok = a
                    .spend()
                    .spend_auth_sig()
                    .as_ref()
                    .is_some_and(|s| a.spend().rk().verify(&sighash, s).is_ok());
                if !sig_ok {
                    push(fl, "dummy_signature");
                }
            }
        }
        if !head_spent {
            push(fl, "head_not_spent");
        }
        if real > 1 + plan.max_inputs as u64 {
            push(fl, "too_many_inputs");
        }
        // Fee.
        let n_actions = b.actions().len() as u64;
        let fee = zip317_fee(n_actions);
        let (mag, sign) = b.value_sum().magnitude_sign();
        if matches!(sign, orchard::value::Sign::Negative) || mag != fee {
            push(fl, "value_balance");
        }
        if mag != fee {
            push(fl, "fee_shape");
        }
        if fee > fee_pool_left(plan, &head.memo) + swept {
            push(fl, "fee_pool");
        }
        let expected_memo = head.memo.advance(&Transition {
            records_paid: pr.records_paid,
            records_digest: pr.records_digest,
            network_fee: fee,
            fee_topups_swept: swept,
        });
        let total_pay: u64 = pr.lines.iter().map(|l| l.1).sum();
        let expected_head_value = value_in.checked_sub(total_pay + fee);
        // Outputs.
        let mut vault_outs = vec![];
        let mut value_outs = vec![];
        let mut zero_outs = 0u64;
        let empty = tx::empty_memo();
        for a in b.actions() {
            if checks::check_cmx(a).is_err() {
                push(fl, "cmx_mismatch");
            }
            let o = a.output();
            let recipient = o.recipient().unwrap();
            let v = o.value().unwrap().inner();
            if let Some(scope) = fvk.scope_for_address(&recipient) {
                vault_outs.push((a, scope, v));
            } else if v > 0 {
                value_outs.push((a, recipient.to_raw_address_bytes(), v));
            } else {
                zero_outs += 1;
                if let Err(e) = checks::check_ciphertexts(a, &empty, &keys.padding_ovk()) {
                    push(fl, ciphertext_failure(&e));
                }
            }
        }
        if vault_outs.len() != 1 {
            push(fl, "vault_outputs_count");
        } else {
            let (a, scope, v) = vault_outs[0];
            if scope != Scope::Internal {
                push(fl, "head_scope");
            }
            if Some(v) != expected_head_value {
                push(fl, "head_value");
            }
            match (expected_memo, checks::recover_memo(a)) {
                (_, None) => push(fl, "enc_ciphertext"),
                (Some(m), Some(got)) if got == m.encode() => {
                    if let Err(e) =
                        checks::check_ciphertexts(a, &m.encode(), &keys.ovk(Scope::Internal))
                    {
                        push(fl, ciphertext_failure(&e));
                    }
                }
                _ => push(fl, "head_memo_advance"),
            }
        }
        // Payouts: exactly the plan's lines (receiver, amount), fixed empty memo.
        let mut expected: Vec<(Receiver, u64)> = pr.lines.clone();
        for (a, to, v) in &value_outs {
            if let Some(i) = expected.iter().position(|(r, x)| r == to && x == v) {
                expected.remove(i);
                match checks::recover_memo(a) {
                    None => push(fl, "enc_ciphertext"),
                    Some(m) if m != empty => push(fl, "payout_memo"),
                    Some(_) => {
                        if let Err(e) =
                            checks::check_ciphertexts(a, &empty, &keys.ovk(Scope::External))
                        {
                            push(fl, ciphertext_failure(&e));
                        }
                    }
                }
            } else {
                push(fl, "unexpected_value_output");
            }
        }
        if !expected.is_empty() {
            push(fl, "payout_missing");
        }
        // Shape: padding only up to the action target.
        let fixed = pr.lines.len() as u64 + 1;
        let want_actions = (plan.action_target as u64).max(real).max(fixed);
        if n_actions != want_actions {
            push(fl, "action_count");
        }
        if zero_outs != want_actions - fixed {
            push(fl, "padding_count");
        }
        // Reserve after (rule 6): new head + credited and top-up notes left unspent.
        let left: u64 = elig
            .iter()
            .filter(|(nf, _)| !spent_elig.contains(*nf))
            .map(|(_, n)| n.value)
            .sum();
        let new_head = if vault_outs.len() == 1 {
            vault_outs[0].2
        } else {
            0
        };
        if new_head + left < plan.claims_after {
            push(fl, "reserve");
        }
        // Proof.
        let proof_ok = b.zkproof().as_ref().is_some_and(|p| {
            let inst: Option<Vec<_>> = b
                .actions()
                .iter()
                .map(|a| {
                    orchard::circuit::Instance::from_parts(
                        *b.anchor(),
                        a.cv_net().clone(),
                        *a.spend().nullifier(),
                        a.spend().rk().clone(),
                        *a.output().cmx(),
                        *b.flags(),
                    )
                })
                .collect();
            inst.is_some_and(|i| p.verify(tx::verifying_key(), &i).is_ok())
        });
        if !proof_ok {
            push(fl, "proof");
        }
        (f2, fee)
    });
    match r {
        Ok((f2, _)) => {
            for x in f2 {
                fail(&mut f, &x);
            }
        }
        Err(_) => fail(&mut f, "pczt_parse"),
    }
    done(f, sighash)
}

/// Action indices of this app's real (vault-key) spends in `pczt`, with their alpha.
pub fn vault_spends(keys: &AppKeys, pczt: &Pczt) -> Result<Vec<(u32, [u8; 32])>, Error> {
    tx::with_ironwood(pczt, |b| {
        b.actions()
            .iter()
            .enumerate()
            .filter(|(_, a)| checks::rk_matches(a, keys.fvk()))
            .map(|(i, a)| (i as u32, a.spend().alpha().unwrap().to_repr()))
            .collect()
    })
}
