//! Payout planning, mark_paid and paid-rollback halts against the real ledger
//! on LiteSVM, with scripted head chains standing in for each operator's zcashd.

mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use ed25519_dalek::SigningKey;
use solana_keypair::Keypair;
use zol_gateway::payout::{genesis_only, plan, record_broadcast, Wait};
use zol_ledger::state::SettlementState;
use zol_tests::*;
use zol_types::api::{HeadChain, HeadLink};

fn sk(b: u8) -> SigningKey {
    SigningKey::from_bytes(&[b; 32])
}

/// Two players credited at full depth, both exit, one settlement record.
fn settled() -> (World, [u8; 32], Vec<Keypair>) {
    let mut w = World::new();
    w.mine(1);
    let g = w.genesis(5_000_000);
    let mut players = Vec::new();
    for (b, addr) in [(1u8, [7u8; 43]), (2u8, [8u8; 43])] {
        w.deposit(&memo(&sk(b), 10_000, addr).encode(), DENOM, None);
        players.push(Keypair::new_from_array([b; 32]));
    }
    w.mine(100);
    assert_eq!(w.coordinate().credited.len(), 2);
    for p in &players {
        ok!(w.ledger.env.request_exit(p));
    }
    let keys: Vec<Pubkey> = players.iter().map(solana_signer::Signer::pubkey).collect();
    ok!(w.ledger.env.settle(&keys));
    (w, g.cmx, players)
}

fn set_heads(w: &World, chain: &HeadChain) {
    for n in &w.nodes {
        n.set_head(&app_id(), chain.clone()).unwrap();
    }
}

#[test]
fn plan_pays_the_next_record_to_memo_addresses() {
    let (w, genesis, _) = settled();
    set_heads(&w, &genesis_only(app_id(), genesis, 5_000_000, 2));
    let now = w.ledger.env.now;
    let p = plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap().unwrap();
    assert_eq!(p.plan.records.len(), 1);
    let r = &p.plan.records[0];
    assert_eq!(r.sequence, 0);
    let fee = |a: u64| a - a * 30 / 10_000 - 10_000;
    let net = DENOM - DENOM * 30 / 10_000;
    assert_eq!(r.payouts.iter().map(|l| (l.to.clone(), l.amount)).collect::<Vec<_>>(), vec![
        ([7u8; 43].to_vec(), fee(net)),
        ([8u8; 43].to_vec(), fee(net)),
    ]);
    assert_eq!(p.plan.credited.len(), 2);
    assert_eq!(p.plan.claims_after, 0, "everything owed is in this record");
    assert_eq!((p.plan.action_target, p.plan.max_inputs), (4, 3));
    assert!(!p.needs_all);
    // Every operator derives the same plan independently.
    for n in &w.nodes[1..] {
        assert_eq!(plan(n, &w.ledger, &app_id(), now).unwrap().unwrap().plan, p.plan);
    }
}

#[test]
fn nothing_to_pay_and_pending_transitions_wait() {
    let (w, genesis, _) = settled();
    // The head chain already paid record 0: nothing left.
    let mut chain = genesis_only(app_id(), genesis, 5_000_000, 2);
    chain.chain.push(HeadLink { paid_count: 1, position: 1, txid: [0xAB; 32], height: 105, ..chain.chain[0].clone() });
    set_heads(&w, &chain);
    let now = w.ledger.env.now;
    assert_eq!(plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap(), Err(Wait::Nothing));

    // A broadcast transition blocks the next one until mined or expired.
    set_heads(&w, &genesis_only(app_id(), genesis, 5_000_000, 2));
    let p = plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap().unwrap();
    let tip = w.nodes[0].store.tip().unwrap().unwrap();
    record_broadcast(&w.nodes[0], &app_id(), &p, &[0xCD; 32], tip + 5, now).unwrap();
    assert_eq!(plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap(), Err(Wait::Pending([0xCD; 32])));
    let mut w = w;
    w.mine(5);
    assert!(plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap().is_ok(), "expired: may propose again");
}

#[test]
fn limits_delay_and_cap_large_payouts() {
    let (mut w, genesis, _) = settled();
    for n in &mut w.nodes {
        n.cfg.payout.payout_cap = 500_000;
        n.cfg.payout.large_delay_secs = 600;
    }
    set_heads(&w, &genesis_only(app_id(), genesis, 5_000_000, 2));
    let now = w.ledger.env.now;
    assert_eq!(plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap(), Err(Wait::LargeDelay { sequence: 0 }));
    let p = plan(&w.nodes[0], &w.ledger, &app_id(), now + 601).unwrap().unwrap();
    assert!(p.needs_all, "large lines need every operator");

    for n in &mut w.nodes {
        n.cfg.payout.payout_cap = u64::MAX;
        n.cfg.payout.hourly_cap = 1_000_000;
    }
    assert_eq!(plan(&w.nodes[1], &w.ledger, &app_id(), now).unwrap(), Err(Wait::HourlyCap));
}

#[test]
fn frozen_or_unreconciled_apps_are_not_paid() {
    let (mut w, genesis, _) = settled();
    set_heads(&w, &genesis_only(app_id(), genesis, 5_000_000, 2));
    let now = w.ledger.env.now;
    ok!(w.ledger.env.freeze(0));
    assert_eq!(plan(&w.nodes[0], &w.ledger, &app_id(), now).unwrap(), Err(Wait::AppNotActive));
}

#[test]
fn mark_paid_after_payout_depth_then_halt_if_rolled_back() {
    let (mut w, genesis, _) = settled();
    let mut chain = genesis_only(app_id(), genesis, 5_000_000, 2);
    let paid_at = w.height + 1;
    w.mine(1);
    chain.chain.push(HeadLink { paid_count: 1, position: 1, txid: [0xAB; 32], height: paid_at, ..chain.chain[0].clone() });
    set_heads(&w, &chain);

    // Below payout depth: not yet.
    w.mine(50);
    assert!(w.coordinate().marked_paid.is_empty());
    w.mine(50);
    let r = w.coordinate();
    assert_eq!(r.marked_paid, vec![0], "{r:?}");
    let s: zol_ledger::state::Settlement = w.ledger.env.account(&settlement_pda(&app_pda(&TEST_APP), 0));
    assert_eq!(s.state, SettlementState::Paid);
    assert_eq!(w.ledger.env.app_state().unpaid_total, 0);
    w.ledger.env.check_invariants();

    // The paying transition disappears from the head chain: every node halts.
    set_heads(&w, &genesis_only(app_id(), genesis, 5_000_000, 2));
    for n in &w.nodes {
        assert!(n.halted().unwrap().unwrap().contains("PaidRolledBack"));
    }
}

#[test]
fn head_chain_fault_halts() {
    let (w, genesis, _) = settled();
    let mut chain = genesis_only(app_id(), genesis, 5_000_000, 2);
    chain.fault = Some("transition with two vault outputs".into());
    w.nodes[1].set_head(&app_id(), chain).unwrap();
    assert!(w.nodes[1].halted().unwrap().unwrap().contains("HeadChainFault"));
    assert!(plan(&w.nodes[1], &w.ledger, &app_id(), w.ledger.env.now).is_err());
}
