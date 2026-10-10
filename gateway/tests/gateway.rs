//! Gateway end to end against the real ledger program on LiteSVM, with a
//! three-operator 2-of-3 FROST attestor and a scripted Zcash chain.

mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use ed25519_dalek::SigningKey;
use zol_gateway::chain::{ChainEvent, Scope};
use zol_gateway::ledger::Ledger;
use zol_gateway::node::{GenesisProblem, GuardAction, HaltReason};
use zol_gateway::reconcile::Mismatch;
use zol_gateway::store::NoteState;
use zol_ledger::state::{AppState, DepositState};
use zol_tests::*;

fn sk(b: u8) -> SigningKey {
    SigningKey::from_bytes(&[b; 32])
}

fn state_of(w: &World, node: usize, id: &[u8; 32]) -> NoteState {
    w.nodes[node].store.note_by_deposit_id(id).unwrap().unwrap().state
}

#[test]
fn early_credit_within_float_then_full_depth_credit() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(1_500_000);
    let a = memo(&sk(1), 10_000, [7; 43]);
    let b = memo(&sk(2), 10_000, [8; 43]);
    w.mine(5);
    let na = w.deposit(&a.encode_text().unwrap().0, DENOM, None); // text memo, as from a wallet link
    let nb = w.deposit(&b.encode(), DENOM, None); // binary memo
    let ida = w.secrets().deposit_id(&na.cmx);
    let idb = w.secrets().deposit_id(&nb.cmx);

    // Under early depth: nothing.
    w.mine(8);
    assert!(w.coordinate().credited.is_empty());

    // Early depth: the float (1.5M) covers one early credit of 1M, not two.
    w.mine(1);
    let r = w.coordinate();
    assert_eq!(r.credited.len(), 1, "{r:?}");
    assert!(r.failed.is_empty(), "{r:?}");
    let first = r.credited[0];
    let ledger_dep: zol_ledger::state::Deposit = w.ledger.env.account(&deposit_pda(&first));
    assert_eq!(ledger_dep.state, DepositState::Credited);
    for i in 0..3 {
        assert_eq!(state_of(&w, i, &first), NoteState::Credited);
    }

    // At full depth the second one is credited too.
    w.mine(90);
    let r = w.coordinate();
    assert_eq!(r.credited.len(), 1, "{r:?}");
    let mut both = vec![first, r.credited[0]];
    both.sort();
    let mut want = vec![ida, idb];
    want.sort();
    assert_eq!(both, want);

    let bal = w.ledger.env.balance(&Pubkey::new_from_array(a.session_key));
    assert_eq!(bal.amount, DENOM - DENOM * 30 / 10_000);
    w.ledger.env.check_invariants();

    // Every operator reconciles cleanly.
    for n in &w.nodes {
        assert!(n.guard(&mut w.ledger).unwrap().is_empty());
    }
    assert_eq!(w.ledger.env.app_state().state, AppState::Active);
}

#[test]
fn invalid_memos_are_voided_and_refunds_become_due() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    let expired = memo(&sk(3), 5, [9; 43]);
    let mut forged = memo(&sk(4), 10_000, [9; 43]);
    forged.payout_address = [0x66; 43]; // changed after signing
    let n1 = w.deposit(&expired.encode(), DENOM, None);
    let n2 = w.deposit(&forged.encode(), DENOM, None);
    let n3 = w.deposit(&memo(&sk(5), 10_000, [9; 43]).encode(), 1_234, None); // not a denomination
    w.mine(10);
    let r = w.coordinate();
    assert_eq!(r.voided.len(), 3, "{r:?}");
    for n in [&n1, &n2, &n3] {
        let d: zol_ledger::state::Deposit = w.ledger.env.account(&deposit_pda(&w.secrets().deposit_id(&n.cmx)));
        assert_eq!(d.state, DepositState::Voided);
    }
    let due = w.nodes[1].refunds_due(&w.ledger, &app_id()).unwrap();
    assert_eq!(due.len(), 3);
    // Refund goes to the address in the deposit's own memo.
    assert!(due.iter().any(|(n, to)| n.note.cmx == n2.cmx && *to == [0x66; 43]));
    // Once the refund spends the note it is no longer due.
    w.feed(ChainEvent::Spent { app: app_id(), nullifier: n1.nullifier, txid: [1; 32], height: w.height + 1 }, None);
    assert_eq!(w.nodes[1].refunds_due(&w.ledger, &app_id()).unwrap().len(), 2);
    w.ledger.env.check_invariants();
}

/// A held deposit moves only when a threshold of operators each release it,
/// and its refund only when they name the same address.
#[test]
fn held_deposit_is_refunded_only_when_operators_release_it_alike() {
    let mut w = World::new();
    let mut empty = [0u8; 512];
    empty[0] = 0xF6;
    let n = w.deposit(&empty, 1_000_000, None);
    w.mine(10);
    let id = w.secrets().deposit_id(&n.cmx);
    w.coordinate();
    assert_eq!(state_of(&w, 0, &id), NoteState::Held);

    // One operator alone: no void.
    let to = [7u8; 43];
    w.nodes[0].store.put_release(&app_id(), &n.cmx, &to, 0).unwrap();
    w.coordinate();
    assert!(w.ledger.deposit(&id).unwrap().is_none(), "one release is not a threshold");

    // A second one: voided on the ledger; both refund to the released address.
    w.nodes[1].store.put_release(&app_id(), &n.cmx, &to, 0).unwrap();
    let r = w.coordinate();
    assert_eq!(r.voided.len(), 1, "{r:?}");
    for i in [0, 1] {
        let due = w.nodes[i].refunds_due(&w.ledger, &app_id()).unwrap();
        assert_eq!(due.iter().map(|(n, t)| (n.note.cmx, *t)).collect::<Vec<_>>(), vec![(n.cmx, to)]);
    }
    // The operator that released nothing has no refund for it.
    assert!(w.nodes[2].refunds_due(&w.ledger, &app_id()).unwrap().is_empty());
    // An operator that names another address would not co-sign the refund.
    w.nodes[1].store.put_release(&app_id(), &n.cmx, &[8u8; 43], 0).unwrap();
    assert_ne!(w.nodes[1].refunds_due(&w.ledger, &app_id()).unwrap()[0].1, to);
}

/// Serves a stale `App` for the first `stale` reads, as if a credit finalized
/// between the guardian's reads of the app and of its deposit list.
struct StaleApp<'a> {
    inner: &'a mut SvmLedger,
    old: zol_ledger::state::App,
    stale: std::cell::Cell<u32>,
}

impl Ledger for StaleApp<'_> {
    fn config(&self) -> zol_gateway::ledger::Result<zol_ledger::state::Config> {
        self.inner.config()
    }
    fn app(&self, p: &Pubkey) -> zol_gateway::ledger::Result<Option<zol_ledger::state::App>> {
        if self.stale.get() > 0 {
            self.stale.set(self.stale.get() - 1);
            return Ok(Some(self.old.clone()));
        }
        self.inner.app(p)
    }
    fn deposit(&self, id: &[u8; 32]) -> zol_gateway::ledger::Result<Option<zol_ledger::state::Deposit>> {
        self.inner.deposit(id)
    }
    fn deposits(&self, p: &Pubkey) -> zol_gateway::ledger::Result<Vec<([u8; 32], zol_ledger::state::Deposit)>> {
        self.inner.deposits(p)
    }
    fn balance(&self, p: &Pubkey, s: &Pubkey) -> zol_gateway::ledger::Result<Option<zol_ledger::state::Balance>> {
        self.inner.balance(p, s)
    }
    fn settlements(&self, p: &Pubkey) -> zol_gateway::ledger::Result<Vec<zol_ledger::state::Settlement>> {
        self.inner.settlements(p)
    }
    fn now(&self) -> zol_gateway::ledger::Result<i64> {
        self.inner.now()
    }
    fn submit_credit(&mut self, p: &Pubkey, a: zol_ledger::CreditArgs, s: [u8; 64]) -> zol_gateway::ledger::Result<()> {
        self.inner.submit_credit(p, a, s)
    }
    fn submit_void(&mut self, p: &Pubkey, d: [u8; 32], s: [u8; 64]) -> zol_gateway::ledger::Result<()> {
        self.inner.submit_void(p, d, s)
    }
    fn submit_paid(&mut self, p: &Pubkey, q: u64, s: [u8; 64]) -> zol_gateway::ledger::Result<()> {
        self.inner.submit_paid(p, q, s)
    }
    fn freeze(&mut self, g: &[u8; 32], p: &Pubkey) -> zol_gateway::ledger::Result<()> {
        self.inner.freeze(g, p)
    }
}

/// Found by the multi-host test under load: the app account read before a
/// credit, the deposit list after it. The guardian must not freeze on that.
#[test]
fn a_credit_landing_between_guardian_reads_does_not_freeze() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    let program = w.nodes[0].apps[&app_id()].program;
    let old = w.ledger.app(&program).unwrap().unwrap();
    w.deposit(&memo(&sk(3), 10_000, [9; 43]).encode(), DENOM, None);
    w.mine(100);
    assert_eq!(w.coordinate().credited.len(), 1);
    // One stale read: the guardian re-reads the app after the list and sees a
    // consistent pair. Nothing is even marked.
    let mut l = StaleApp { inner: &mut w.ledger, old: old.clone(), stale: std::cell::Cell::new(1) };
    let a = w.nodes[2].guard(&mut l).unwrap();
    assert!(a.is_empty(), "{a:?}");
    assert!(w.nodes[2].suspect.borrow().is_empty());
    // A backend that keeps serving the old app (a load-balanced RPC): one pass
    // marks the app; the next, reading consistently, clears it. No freeze.
    let mut l = StaleApp { inner: &mut w.ledger, old, stale: std::cell::Cell::new(5) };
    assert!(w.nodes[2].guard(&mut l).unwrap().is_empty());
    assert!(w.nodes[2].suspect.borrow().contains(&app_id()));
    assert!(w.nodes[2].guard(&mut w.ledger).unwrap().is_empty());
    assert!(w.nodes[2].suspect.borrow().is_empty());
    assert_eq!(w.ledger.env.app_state().state, AppState::Active);
}

#[test]
fn a_note_seen_by_one_operator_is_not_signed() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    let n = w.deposit(&memo(&sk(1), 10_000, [7; 43]).encode(), DENOM, Some(&[0]));
    w.mine(10);
    let r = w.coordinate();
    assert!(r.credited.is_empty());
    assert_eq!(r.failed.len(), 1, "{r:?}");
    assert!(w.ledger.env.try_account::<zol_ledger::state::Deposit>(&deposit_pda(&w.secrets().deposit_id(&n.cmx))).is_none());
}

#[test]
fn forged_credit_freezes_the_app() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    w.deposit(&memo(&sk(1), 10_000, [7; 43]).encode(), DENOM, None);
    w.mine(10);
    assert_eq!(w.coordinate().credited.len(), 1);

    // A compromised attestor quorum credits a deposit that has no note.
    let rogue = SigningKey::from_bytes(&[0xBA; 32]);
    let args = w.ledger.env.credit_args([0xF0; 32], 10 * DENOM, Pubkey::new_unique(), [1; 32], 0);
    ok!(w.ledger.env.set_config(zol_ledger::SetConfigArgs {
        new_admin: None,
        attestor: Some(Pubkey::new_from_array(rogue.verifying_key().to_bytes())),
        params: None,
        paused: None,
    }));
    ok!(w.ledger.env.credit_with(args, &rogue));

    // The first failing pass only marks the app; the second freezes it.
    assert!(w.nodes[2].guard(&mut w.ledger).unwrap().is_empty());
    assert_eq!(w.ledger.env.app_state().state, AppState::Active);
    let actions = w.nodes[2].guard(&mut w.ledger).unwrap();
    match &actions[..] {
        [GuardAction::Froze { mismatches, .. }] => {
            assert!(mismatches.contains(&Mismatch::NoNote { deposit_id: [0xF0; 32] }));
            assert!(mismatches.iter().any(|m| matches!(m, Mismatch::CreditedTotal { .. })));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(w.ledger.env.app_state().state, AppState::Frozen);
}

#[test]
fn misbound_credit_of_a_real_note_freezes_the_app() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    let m = memo(&sk(1), 10_000, [7; 43]);
    let n = w.deposit(&m.encode(), DENOM, None);
    w.mine(10);
    // A rogue attestor credits the real note first, to its own session and payout commitment.
    let rogue = SigningKey::from_bytes(&[0xBB; 32]);
    ok!(w.ledger.env.set_config(zol_ledger::SetConfigArgs {
        new_admin: None,
        attestor: Some(Pubkey::new_from_array(rogue.verifying_key().to_bytes())),
        params: None,
        paused: None,
    }));
    let id = w.secrets().deposit_id(&n.cmx);
    let args = w.ledger.env.credit_args(id, DENOM, Pubkey::new_unique(), [0xEE; 32], 0);
    ok!(w.ledger.env.credit_with(args, &rogue));

    assert!(w.nodes[0].guard(&mut w.ledger).unwrap().is_empty(), "one failing pass only marks the app");
    let actions = w.nodes[0].guard(&mut w.ledger).unwrap();
    assert!(matches!(&actions[..], [GuardAction::Froze { mismatches, .. }] if mismatches.contains(&Mismatch::DigestDiffers { deposit_id: id })), "{actions:?}");
    assert_eq!(w.ledger.env.app_state().state, AppState::Frozen);
}

#[test]
fn memo_dropped_by_wallet_is_credited_from_registered_intent() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    let m = memo(&sk(6), 10_000, [7; 43]);
    // The page registers the signed memo with every operator; the wallet drops it.
    let idx = (session_index(app_id()))(&m.session_key);
    for n in &w.nodes {
        assert_eq!(n.register_intent(&app_id(), &m.encode()).unwrap(), idx);
    }
    let mut empty = [0u8; 512];
    empty[0] = 0xF6;
    let note = w.note(&empty, DENOM, Scope::External, idx);
    w.feed(ChainEvent::Note(note.clone()), None);
    // A memo-less note at an unregistered receiver is only held.
    let stray = w.note(&empty, DENOM, Scope::External, 12345);
    w.feed(ChainEvent::Note(stray.clone()), None);
    w.mine(10);
    let r = w.coordinate();
    assert_eq!(r.credited, vec![w.secrets().deposit_id(&note.cmx)], "{r:?}");
    assert_eq!(state_of(&w, 0, &w.secrets().deposit_id(&stray.cmx)), NoteState::Held);
    for n in &w.nodes {
        assert!(n.guard(&mut w.ledger).unwrap().is_empty());
    }
    // A forged intent is refused.
    let mut bad = m.clone();
    bad.expiry_height += 1;
    assert!(w.nodes[0].register_intent(&app_id(), &bad.encode()).is_err());
}

#[test]
fn early_credit_rollback_is_absorbed_by_float_or_halts() {
    // Float 1.2M covers the 1M early credit; when it rolls back the float absorbs it.
    let mut w = World::new();
    w.mine(1);
    w.genesis(1_200_000);
    w.mine(20);
    let n = w.deposit(&memo(&sk(1), 10_000, [7; 43]).encode(), DENOM, None);
    w.mine(10);
    assert_eq!(w.coordinate().credited.len(), 1);
    let id = w.secrets().deposit_id(&n.cmx);
    let reorg_to = n.height - 1;
    w.feed(ChainEvent::Rewind { to_height: reorg_to }, None);
    for node in &w.nodes {
        assert!(node.halted().unwrap().is_none(), "shallow reorg does not halt");
        let rec = node.store.note_by_deposit_id(&id).unwrap().unwrap();
        assert!(rec.rolled_back && rec.state == NoteState::Credited, "record is kept for reconciliation");
        // Reconciliation still matches; the float has absorbed the loss.
        assert!(node.guard(&mut w.ledger).unwrap().is_empty());
        assert!(node.halted().unwrap().is_none());
    }

    // If the reserve cannot cover claims after a rollback (here the head was also
    // spent, as a payout would), every node halts and signs nothing.
    let mut w = World::new();
    w.mine(1);
    w.genesis(1_000_000);
    w.mine(20);
    let n = w.deposit(&memo(&sk(2), 10_000, [7; 43]).encode(), DENOM, None);
    w.mine(10);
    assert_eq!(w.coordinate().credited.len(), 1);
    w.feed(ChainEvent::Rewind { to_height: n.height - 1 }, None);
    let head = w.nodes[0].store.notes(&app_id()).unwrap().into_iter().find(|r| r.note.scope == Scope::Internal).unwrap();
    w.feed(ChainEvent::Spent { app: app_id(), nullifier: head.note.nullifier, txid: [3; 32], height: w.height }, None);
    for node in &w.nodes {
        let _ = node.guard(&mut w.ledger).unwrap();
        assert!(matches!(node.halted().unwrap(), Some(r) if r.contains("ReserveShort")));
        assert!(node.own_statements(&w.ledger, &app_id()).is_err(), "a halted node signs nothing");
    }
}

#[test]
fn deep_reorg_and_full_depth_rollback_halt() {
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    w.mine(5);
    let n = w.deposit(&memo(&sk(1), 10_000, [7; 43]).encode(), DENOM, None);
    w.mine(120);
    assert_eq!(w.coordinate().credited.len(), 1);
    w.feed(ChainEvent::Rewind { to_height: n.height - 1 }, None);
    for node in &w.nodes {
        let h = node.halted().unwrap().unwrap();
        assert!(h.contains("DeepReorg"), "{h}");
        node.clear_halt().unwrap();
    }
    let _ = HaltReason::TipStalled { since: 0 };
}

#[test]
fn tip_stall_halts() {
    let mut w = World::new();
    w.mine(3);
    let t = w.ledger.env.now;
    w.nodes[0].check_stall(t + 100).unwrap();
    assert!(w.nodes[0].halted().unwrap().is_none());
    w.nodes[0].check_stall(t + 1_801).unwrap();
    assert!(w.nodes[0].halted().unwrap().unwrap().contains("TipStalled"));
}

#[test]
fn signed_decision_is_never_reversed() {
    // Operator 0 already signed a credit for this deposit (e.g. a quorum that
    // stalled before relaying). When the memo later expires, the others agree on
    // a void but operator 0's log refuses, so 2-of-3 needs both others.
    let mut w = World::new();
    w.mine(1);
    w.genesis(5_000_000);
    let m = memo(&sk(1), 40, [7; 43]);
    let n = w.deposit(&m.encode(), DENOM, None);
    let id = w.secrets().deposit_id(&n.cmx);
    w.nodes[0].store.record_decision(&[b"dep:".as_slice(), &id].concat(), "credit", &[1; 32]).unwrap();
    w.mine(60); // past the memo's expiry: everyone now derives a void
    let r = w.coordinate();
    assert_eq!(r.voided, vec![id], "{r:?}");
    // Operator 0 did not contribute: its log still holds only the credit.
    let (kind, _) = w.nodes[0].store.decision(&[b"dep:".as_slice(), &id].concat()).unwrap().unwrap();
    assert_eq!(kind, "credit");
}

#[test]
fn guardians_cancel_an_activation_without_a_valid_genesis_head() {
    let mut env = Env::bare();
    ok!(env.initialize());
    ok!(env.register_test_app());
    ok!(env.activate(&TEST_APP)); // genesis_head = [0x9E; 32], which nobody has seen
    let mut w = World::with_env(env);
    w.mine(150);
    let actions = w.nodes[1].guard(&mut w.ledger).unwrap();
    assert!(matches!(&actions[..], [GuardAction::CancelledActivation { problem: GenesisProblem::Missing, .. }]), "{actions:?}");
    assert_eq!(w.ledger.env.app_state().state, AppState::Pending);
}

#[test]
fn operators_on_different_chains_halt() {
    let mut w = World::new();
    w.mine(30);
    // Operator 2's node followed a different fork from height 15.
    w.nodes[2].apply(&ChainEvent::Rewind { to_height: 14 }, w.ledger.env.now).unwrap();
    for h in 15..=30 {
        w.nodes[2].apply(&ChainEvent::Block { height: h, hash: block_hash(h, 9) }, w.ledger.env.now).unwrap();
    }
    let reports: Vec<_> = w.nodes.iter().map(|n| n.chain_report().unwrap().unwrap()).collect();
    assert_eq!(reports[0].0, 21);
    for n in &w.nodes {
        n.check_agreement(&reports).unwrap();
        assert!(n.halted().unwrap().unwrap().contains("ChainDisagreement"));
    }
    // Agreeing operators do not halt.
    let mut w = World::new();
    w.mine(30);
    let reports: Vec<_> = w.nodes.iter().map(|n| n.chain_report().unwrap().unwrap()).collect();
    for n in &w.nodes {
        n.check_agreement(&reports).unwrap();
        assert!(n.halted().unwrap().is_none());
    }
}
