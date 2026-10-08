//! Happy paths through all three state machines.

use solana_signer::Signer;
use zol_ledger::state::{DepositState, PotState, SettlementState};
use zol_tests::*;

const POT: [u8; 32] = [0x01; 32];

#[test]
fn full_session_deposit_to_paid() {
    let mut env = Env::new();
    let alice = env.player(1_000_000);
    let bob = env.player(1_000_000);
    env.check_invariants();

    // Entry fee 0.3%: 1_000_000 -> 997_000 each, 3_000 each to the fee pool.
    assert_eq!(env.balance(&alice.pubkey()).amount, 997_000);
    let app = env.app_state();
    assert_eq!((app.liability, app.credited_total, app.fee_pool, app.deposit_count), (1_994_000, 2_000_000, 6_000, 2));

    ok!(env.open_pot(POT, 600));
    ok!(env.buy_in(POT, &alice, 500_000));
    ok!(env.buy_in(POT, &bob, 500_000));
    ok!(env.buy_in(POT, &alice, 100_000)); // tops up the same seat
    env.check_invariants();
    let expected = env.expected(POT);
    assert_eq!(expected.len(), 2);
    assert_eq!(expected[0].amount, 600_000);

    // Real delegation CPI into MagicBlock's delegation program.
    ok!(env.delegate(POT, expected, 600));
    let pot_key = pot_pda(&app_pda(&TEST_APP), &POT);
    assert_eq!(env.raw(&pot_key).unwrap().owner, DLP, "pot belongs to the delegation program while out");
    let r: st::PotReceipt = env.account(&receipt_pda(&pot_key));
    assert_eq!(r.validator, VALIDATOR, "validator pinned in the receipt");
    env.check_invariants();

    // Not returned yet: no cash-out, no refund.
    assert_zerr!(env.cash_out(POT, &[alice.pubkey(), bob.pubkey()]), PotNotReturned);
    assert_zerr!(env.refund_pot(POT, &[alice.pubkey(), bob.pubkey()]), RefundNotAllowed);

    // Rollup: Alice wins 300_000 of Bob's seat; the validator commits and returns the pot.
    env.simulate_return(POT, |p| {
        p.amounts[0] += 300_000;
        p.amounts[1] -= 300_000;
        p.delegated = false;
    });
    ok!(env.cash_out(POT, &[alice.pubkey(), bob.pubkey()]));
    env.check_invariants();
    assert_eq!(env.balance(&alice.pubkey()).amount, 997_000 - 600_000 + 900_000);
    assert_eq!(env.balance(&bob.pubkey()).amount, 997_000 - 500_000 + 200_000);
    let r: st::PotReceipt = env.account(&receipt_pda(&pot_key));
    assert_eq!(r.state, PotState::CashedOut);
    // A receipt is used once.
    assert_zerr!(env.cash_out(POT, &[alice.pubkey(), bob.pubkey()]), BadPotState);

    // Exit.
    ok!(env.request_exit(&alice));
    ok!(env.request_exit(&bob));
    ok!(env.settle(&[alice.pubkey(), bob.pubkey()]));
    env.check_invariants();
    let s: st::Settlement = env.account(&settlement_pda(&app_pda(&TEST_APP), 0));
    assert_eq!(s.state, SettlementState::Unpaid);
    assert_eq!(s.count, 2);
    // payout = amount - 0.3% - 10_000 allowance
    let a_amt: u64 = 1_297_000;
    assert_eq!(s.payouts[0].amount, a_amt - a_amt * 30 / 10_000 - 10_000);
    let app = env.app_state();
    assert_eq!(app.liability, 0);
    assert_eq!(app.unpaid_total, s.total);
    assert_eq!(app.next_settlement, 1);
    // Fee pool = entry fees + exit fees + allowances; everything is accounted for.
    assert_eq!(app.fee_pool + app.unpaid_total, app.credited_total);

    ok!(env.mark_paid(0));
    let app = env.app_state();
    assert_eq!((app.unpaid_total, app.paid_count), (0, 1));
    assert_zerr!(env.mark_paid(0), AlreadyPaid);
}

#[test]
fn void_then_credit_is_impossible_and_vice_versa() {
    let mut env = Env::new();
    let k = solana_keypair::Keypair::new();
    ok!(env.void([0xD1; 32]));
    let d: st::Deposit = env.account(&deposit_pda(&[0xD1; 32]));
    assert_eq!(d.state, DepositState::Voided);
    assert!(env.credit([0xD1; 32], 1_000_000, k.pubkey(), [7; 32]).is_err(), "credit after void");

    ok!(env.credit([0xD2; 32], 1_000_000, k.pubkey(), [7; 32]));
    assert!(env.void([0xD2; 32]).is_err(), "void after credit");
    assert!(env.credit([0xD2; 32], 1_000_000, k.pubkey(), [7; 32]).is_err(), "double credit");
    env.check_invariants();
}
