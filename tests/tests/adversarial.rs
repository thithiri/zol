//! Adversarial tests for the ledger-side rows of the v0.2 and v0.3 change tables
//! and the trust-model bounds on apps, validators and relayers (plan task 2.12).

use ed25519_dalek::SigningKey;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use zol_ledger::state::{PotState, SeatStake};
use zol_tests::*;
use zol_wire::digest::{paid_digest, void_digest, Sha256};

const POT: [u8; 32] = [0xA0; 32];

fn table(env: &mut Env) -> (Keypair, Keypair) {
    let a = env.player(1_000_000);
    let b = env.player(1_000_000);
    ok!(env.open_pot(POT, 600));
    ok!(env.buy_in(POT, &a, 300_000));
    ok!(env.buy_in(POT, &b, 300_000));
    (a, b)
}

fn delegated(env: &mut Env) -> (Keypair, Keypair) {
    let (a, b) = table(env);
    let e = env.expected(POT);
    ok!(env.delegate(POT, e, 600));
    (a, b)
}

// ------------------------------------------------------------------ attestations

#[test]
fn credit_needs_the_attestor_over_the_exact_digest() {
    let mut env = Env::new();
    let k = Keypair::new();
    let args = env.credit_args([1; 32], 1_000_000, k.pubkey(), [7; 32], 0);

    // Wrong key.
    assert_zerr!(env.credit_with(args.clone(), &SigningKey::from_bytes(&[9; 32])), BadAttestation);

    // Right key, but the relayer inflates the amount after signing.
    let digest = Env::credit_digest(&args, &TEST_APP);
    let mut inflated = args.clone();
    inflated.amount = 10_000_000; // a valid denomination, so the signature check is what fails
    let ix = env.credit_ix(inflated);
    let r = env.relayer.insecure_clone();
    let att = env.attestor.clone();
    assert_zerr!(env.send(&[ed25519_ix(&att, &digest), ix], &[&r]), BadAttestation);

    // Right key, right digest, but bound to another app.
    let other = solana_pubkey::Pubkey::new_unique();
    let digest = Env::credit_digest(&args, &other);
    let ix = env.credit_ix(args.clone());
    assert_zerr!(env.send(&[ed25519_ix(&att, &digest), ix], &[&r]), BadAttestation);

    // No precompile instruction at all.
    let ix = env.credit_ix(args.clone());
    assert_zerr!(env.send(&[ix], &[&r]), BadAttestation);

    // A void or paid message is never accepted as a credit.
    for digest in [
        void_digest::<Sha256>(NETWORK, &TEST_APP.to_bytes(), &args.deposit_id),
        paid_digest::<Sha256>(NETWORK, &TEST_APP.to_bytes(), 0),
    ] {
        let ix = env.credit_ix(args.clone());
        assert_zerr!(env.send(&[ed25519_ix(&att, &digest), ix], &[&r]), BadAttestation);
    }

    // Valid.
    ok!(env.credit_with(args, &att));
    env.check_invariants();
}

#[test]
fn precompile_offsets_must_point_inside_itself() {
    let mut env = Env::new();
    let k = Keypair::new();
    let args = env.credit_args([1; 32], 1_000_000, k.pubkey(), [7; 32], 0);
    let digest = Env::credit_digest(&args, &TEST_APP);
    let att = env.attestor.clone();
    // Point the message offset at instruction 1 (the credit itself) instead of u16::MAX.
    let mut pre = ed25519_ix(&att, &digest);
    pre.data[14..16].copy_from_slice(&1u16.to_le_bytes());
    let ix = env.credit_ix(args.clone());
    let r = env.relayer.insecure_clone();
    let res = env.send(&[pre, ix], &[&r]);
    assert!(res.is_err(), "precompile reading another instruction must not pass");
    // Two signatures in one precompile are refused.
    let mut pre = ed25519_ix(&att, &digest);
    pre.data[0] = 2;
    let ix = env.credit_ix(args);
    assert!(env.send(&[pre, ix], &[&r]).is_err());
}

#[test]
fn later_deposit_must_keep_the_payout_address() {
    let mut env = Env::new();
    let k = Keypair::new();
    ok!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32]));
    assert_zerr!(env.credit([2; 32], 1_000_000, k.pubkey(), [8; 32]), PayoutCommitmentMismatch);
    ok!(env.credit([3; 32], 1_000_000, k.pubkey(), [7; 32]));
    env.check_invariants();
}

#[test]
fn mark_paid_needs_attestation_and_order() {
    let mut env = Env::new();
    let a = env.player(1_000_000);
    let b = env.player(1_000_000);
    ok!(env.request_exit(&a));
    ok!(env.settle(&[a.pubkey()]));
    ok!(env.request_exit(&b));
    ok!(env.settle(&[b.pubkey()]));
    assert_zerr!(env.mark_paid(1), OutOfOrder);
    // A paid message for seq 0 cannot mark seq 1 even in order position.
    ok!(env.mark_paid(0));
    let app = app_pda(&TEST_APP);
    let digest = paid_digest::<Sha256>(NETWORK, &TEST_APP.to_bytes(), 0);
    let ix = ledger_ix(
        zol_ledger::accounts::MarkPaid {
            relayer: env.relayer.pubkey(),
            config: config_pda(),
            app,
            settlement: settlement_pda(&app, 1),
            instructions: IX_SYSVAR,
        },
        zol_ledger::instruction::MarkPaid {},
    );
    let r = env.relayer.insecure_clone();
    let att = env.attestor.clone();
    assert_zerr!(env.send(&[ed25519_ix(&att, &digest), ix], &[&r]), BadAttestation);
    ok!(env.mark_paid(1));
}

// ------------------------------------------------------------------ app limits

#[test]
fn app_cannot_seat_a_player_without_the_session_key() {
    let mut env = Env::new();
    let victim = env.player(1_000_000);
    ok!(env.open_pot(POT, 600));
    // The app signs, but the victim's session key does not.
    let mut ix = via_app(env.buy_in_ix(POT, &victim.pubkey(), 500_000));
    for m in ix.accounts.iter_mut() {
        if m.pubkey == victim.pubkey() {
            m.is_signer = false;
        }
    }
    let r = env.relayer.insecure_clone();
    assert!(env.send(&[ix], &[&r]).is_err());
    assert_eq!(env.balance(&victim.pubkey()).amount, 997_000);
}

#[test]
fn player_cannot_enter_a_pot_without_the_app() {
    let mut env = Env::new();
    let p = env.player(1_000_000);
    ok!(env.open_pot(POT, 600));
    // Direct call: the app's zol-app address cannot sign outside the app program.
    let mut ix = env.buy_in_ix(POT, &p.pubkey(), 500_000);
    let fake = Keypair::new();
    for m in ix.accounts.iter_mut() {
        if m.pubkey == app_authority(&TEST_APP) {
            m.pubkey = fake.pubkey();
        }
    }
    let r = env.relayer.insecure_clone();
    assert_zerr!(env.send(&[ix], &[&r, &p, &fake]), BadAppAuthority);
}

#[test]
fn leave_before_delegate_race_fails_closed() {
    let mut env = Env::new();
    let (a, b) = table(&mut env);
    let stale = env.expected(POT);
    // Bob leaves after the app read the receipt but before it delegates.
    ok!(env.leave_pot(POT, &b));
    assert_zerr!(env.delegate(POT, stale, 600), ReceiptMismatch);
    // A substituted player is also refused.
    let mut forged = env.expected(POT);
    forged.push(SeatStake { player: Keypair::new().pubkey(), amount: 300_000 });
    assert_zerr!(env.delegate(POT, forged, 600), ReceiptMismatch);
    let mut forged = env.expected(POT);
    forged[0].amount += 1;
    assert_zerr!(env.delegate(POT, forged, 600), ReceiptMismatch);
    let e = env.expected(POT);
    ok!(env.delegate(POT, e, 600));
    let _ = a;
    env.check_invariants();
}

#[test]
fn leave_is_only_before_start_and_only_own_seat() {
    let mut env = Env::new();
    let (a, b) = table(&mut env);
    let c = env.player(1_000_000);
    assert_zerr!(env.leave_pot(POT, &c), NoSeat);
    ok!(env.leave_pot(POT, &a));
    assert_eq!(env.balance(&a.pubkey()).amount, 997_000);
    // Seat order compacts: Bob is now seat 0 in both receipt and pot.
    let e = env.expected(POT);
    assert_eq!((e.len(), e[0].player), (1, b.pubkey()));
    let pot: st::Pot = env.account(&pot_pda(&app_pda(&TEST_APP), &POT));
    assert_eq!((pot.seat_count, pot.amounts[0], pot.amounts[1]), (1, 300_000, 0));
    ok!(env.delegate(POT, e, 600));
    // Delegated: the pot belongs to the delegation program, so leave fails (owner check).
    assert!(env.leave_pot(POT, &b).is_err());
    // Even with the pot back, a delegated receipt refuses a leave.
    env.simulate_return(POT, |_| {});
    assert_zerr!(env.leave_pot(POT, &b), BadPotState);
    env.check_invariants();
}

#[test]
fn seat_limits_and_exiting_balances() {
    let mut env = Env::new();
    ok!(env.open_pot(POT, 600));
    let max = params().max_seats as usize;
    let players: Vec<_> = (0..=max).map(|_| env.player(1_000_000)).collect();
    for p in &players[..max] {
        ok!(env.buy_in(POT, p, 1_000));
    }
    assert_zerr!(env.buy_in(POT, &players[max], 1_000), PotFull);
    ok!(env.request_exit(&players[max]));
    assert_zerr!(env.buy_in(POT, &players[max], 1_000), BalanceExiting);
    assert_zerr!(env.buy_in(POT, &players[0], 10_000_000), InsufficientBalance);
    assert_zerr!(env.buy_in(POT, &players[0], 0), ZeroAmount);
}

#[test]
fn open_deadline_and_session_deadline_bounds() {
    let mut env = Env::new();
    assert_zerr!(env.open_pot(POT, params().max_open_secs + 1), BadDeadline);
    let (a, b) = table(&mut env);
    let e = env.expected(POT);
    assert_zerr!(env.delegate(POT, e.clone(), params().min_session_secs - 1), BadDeadline);
    assert_zerr!(env.delegate(POT, e.clone(), params().max_session_secs + 1), BadDeadline);
    env.warp(600);
    assert_zerr!(env.delegate(POT, e, 600), OpenDeadlinePassed);
    assert_zerr!(env.buy_in(POT, &a, 1), OpenDeadlinePassed);
    // Open pot past its deadline refunds every buy-in.
    ok!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]));
    assert_eq!(env.balance(&a.pubkey()).amount, 997_000);
    env.check_invariants();
}

#[test]
fn move_seat_rules() {
    let mut env = Env::new();
    let (_a, _b) = table(&mut env);
    // Not delegated yet: seats cannot move.
    let ix = via_app(env.move_seat_ix(POT, 0, 1, 1));
    let r = env.relayer.insecure_clone();
    assert_zerr!(env.send(&[ix], &[&r]), BadPotState);
    let e = env.expected(POT);
    ok!(env.delegate(POT, e, 600));
    // On Solana the delegated pot belongs to the delegation program: no moves here.
    let ix = via_app(env.move_seat_ix(POT, 0, 1, 1));
    assert!(env.send(&[ix], &[&r]).is_err());

    // Simulate the rollup's copy (owned by the ledger, delegated flag set) to test the rules.
    env.simulate_return(POT, |_| {});
    let ix = via_app(env.move_seat_ix(POT, 0, 1, 300_001));
    assert_zerr!(env.send(&[ix], &[&r]), InsufficientBalance);
    let ix = via_app(env.move_seat_ix(POT, 0, 2, 1));
    assert_zerr!(env.send(&[ix], &[&r]), BadSeat);
    let ix = via_app(env.move_seat_ix(POT, 1, 1, 1));
    assert_zerr!(env.send(&[ix], &[&r]), BadSeat);
    // Another signer posing as the app.
    let fake = Keypair::new();
    let mut ix = env.move_seat_ix(POT, 0, 1, 1);
    ix.accounts[0] = AccountMeta::new_readonly(fake.pubkey(), true);
    ix.accounts[2] = AccountMeta::new_readonly(fake.pubkey(), false);
    assert_zerr!(env.send(&[ix], &[&r, &fake]), BadAppAuthority);
    let ix = via_app(env.move_seat_ix(POT, 0, 1, 300_000));
    ok!(env.send(&[ix], &[&r]));
    let pot: st::Pot = env.account(&pot_pda(&app_pda(&TEST_APP), &POT));
    assert_eq!(&pot.amounts[..2], &[0, 600_000]);
}

// ------------------------------------------------------------------ validator faults

#[test]
fn validator_cannot_create_value_or_add_seats() {
    for (name, f) in [
        ("sum up", Box::new(|p: &mut st::Pot| p.amounts[0] += 1) as Box<dyn Fn(&mut st::Pot)>),
        ("sum down", Box::new(|p: &mut st::Pot| p.amounts[1] -= 1)),
        ("extra seat count", Box::new(|p: &mut st::Pot| p.seat_count += 1)),
        ("value past seats", Box::new(|p: &mut st::Pot| {
            p.amounts[0] -= 5;
            p.amounts[5] = 5;
        })),
        ("other pot id", Box::new(|p: &mut st::Pot| p.pot_id = [0xEE; 32])),
        ("other app", Box::new(|p: &mut st::Pot| p.app = solana_pubkey::Pubkey::new_unique())),
    ] {
        let mut env = Env::new();
        let (a, b) = delegated(&mut env);
        env.simulate_return(POT, f);
        let res = env.cash_out(POT, &[a.pubkey(), b.pubkey()]);
        assert_eq!(err_code(&res), Some(zerr(zol_ledger::error::ZolError::PotInvalid)), "{name}");
        // Invalid return: refundable after the session deadline plus grace, not before.
        assert_zerr!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]), RefundNotAllowed);
        env.warp(600 + params().refund_grace_secs);
        ok!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]));
        assert_eq!(env.balance(&a.pubkey()).amount, 997_000, "{name}");
        env.check_invariants();
    }
}

#[test]
fn a_valid_return_can_never_be_refunded() {
    let mut env = Env::new();
    let (a, b) = delegated(&mut env);
    env.simulate_return(POT, |p| {
        p.amounts[0] = 600_000;
        p.amounts[1] = 0;
    });
    // The loser waits out the deadline and tries to reverse the result.
    env.warp(600 + params().refund_grace_secs + 10_000);
    assert_zerr!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]), RefundNotAllowed);
    ok!(env.cash_out(POT, &[a.pubkey(), b.pubkey()]));
    assert_eq!(env.balance(&a.pubkey()).amount, 997_000 + 300_000);
    env.check_invariants();
}

#[test]
fn never_returned_pot_refunds_after_grace_and_late_return_is_ignored() {
    let mut env = Env::new();
    let (a, b) = delegated(&mut env);
    env.warp(600 + params().refund_grace_secs - 1);
    assert_zerr!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]), RefundNotAllowed);
    env.warp(1);
    ok!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]));
    // The pot comes back late with a result: the receipt is spent, nothing changes.
    env.simulate_return(POT, |p| {
        p.amounts[0] = 600_000;
        p.amounts[1] = 0;
    });
    assert_zerr!(env.cash_out(POT, &[a.pubkey(), b.pubkey()]), BadPotState);
    assert_zerr!(env.refund_pot(POT, &[a.pubkey(), b.pubkey()]), RefundNotAllowed);
    let r: st::PotReceipt = env.account(&receipt_pda(&pot_pda(&app_pda(&TEST_APP), &POT)));
    assert_eq!(r.state, PotState::Refunded);
    env.check_invariants();
}

#[test]
fn cash_out_balances_must_match_receipt_order() {
    let mut env = Env::new();
    let (a, b) = delegated(&mut env);
    env.simulate_return(POT, |p| {
        p.amounts[0] = 600_000;
        p.amounts[1] = 0;
    });
    assert_zerr!(env.cash_out(POT, &[b.pubkey(), a.pubkey()]), BadAccount);
    assert_zerr!(env.cash_out(POT, &[a.pubkey()]), BadAccount);
    let c = env.player(1_000_000);
    assert_zerr!(env.cash_out(POT, &[a.pubkey(), c.pubkey()]), BadAccount);
    assert!(env.cash_out(POT, &[a.pubkey(), a.pubkey()]).is_err());
    ok!(env.cash_out(POT, &[a.pubkey(), b.pubkey()]));
    env.check_invariants();
}

#[test]
fn cash_out_rejects_a_pot_from_another_pot_id() {
    let mut env = Env::new();
    let (a, b) = delegated(&mut env);
    env.simulate_return(POT, |_| {});
    // Point the receipt of POT at a different (existing) pot account.
    ok!(env.open_pot([0xB0; 32], 600));
    let app = app_pda(&TEST_APP);
    let mut ix = Instruction {
        program_id: LEDGER,
        accounts: anchor_lang::ToAccountMetas::to_account_metas(
            &zol_ledger::accounts::PotOutcome {
                config: config_pda(),
                app,
                receipt: receipt_pda(&pot_pda(&app, &POT)),
                pot: pot_pda(&app, &[0xB0; 32]),
            },
            None,
        ),
        data: anchor_lang::InstructionData::data(&zol_ledger::instruction::CashOut {}),
    };
    ix.accounts.push(AccountMeta::new(balance_pda(&app, &a.pubkey()), false));
    ix.accounts.push(AccountMeta::new(balance_pda(&app, &b.pubkey()), false));
    let r = env.relayer.insecure_clone();
    assert!(env.send(&[ix], &[&r]).is_err(), "seeds constraint ties pot to receipt");
}

// ------------------------------------------------------------------ exits

#[test]
fn settle_rules() {
    let mut env = Env::new();
    let a = env.player(1_000_000);
    let b = env.player(1_000_000);
    // Leave "poor" with 47,000 by staking the rest in an open pot.
    let poor = env.player(1_000_000);
    ok!(env.open_pot([0xEE; 32], 600));
    ok!(env.buy_in([0xEE; 32], &poor, 950_000));
    assert_zerr!(env.settle(&[a.pubkey()]), NotExiting);
    assert_zerr!(env.request_exit(&poor), BelowMinExit);
    ok!(env.request_exit(&a));
    assert!(env.settle(&[a.pubkey(), a.pubkey()]).is_err(), "duplicate balance");
    assert_zerr!(env.settle(&[]), BadPayoutCount);
    let many: Vec<_> = (0..17).map(|_| a.pubkey()).collect();
    assert!(env.settle(&many).is_err());
    ok!(env.settle(&[a.pubkey()]));
    // Paid out once: the balance is now zero and unmarked.
    assert_eq!(env.balance(&a.pubkey()).amount, 0);
    assert_zerr!(env.settle(&[a.pubkey()]), NotExiting);
    let _ = b;
    env.check_invariants();
}

#[test]
fn auto_exit_marks_after_cash_out() {
    let mut env = Env::new();
    let a = Keypair::new();
    let args = env.credit_args([0x51; 32], 1_000_000, a.pubkey(), [7; 32], zol_wire::memo::FLAG_AUTO_EXIT);
    let att = env.attestor.clone();
    ok!(env.credit_with(args, &att));
    env.svm.airdrop(&a.pubkey(), 1_000_000_000).unwrap();
    let b = env.player(1_000_000);
    ok!(env.open_pot(POT, 600));
    ok!(env.buy_in(POT, &a, 300_000));
    ok!(env.buy_in(POT, &b, 300_000));
    let e = env.expected(POT);
    ok!(env.delegate(POT, e, 600));
    env.simulate_return(POT, |_| {});
    ok!(env.cash_out(POT, &[a.pubkey(), b.pubkey()]));
    assert!(env.balance(&a.pubkey()).exiting);
    assert!(!env.balance(&b.pubkey()).exiting);
}

#[test]
fn delegation_needs_an_explicit_validator() {
    let mut env = Env::new();
    let _ = table(&mut env);
    let e = env.expected(POT);
    let mut ix = env.delegate_ix(POT, e, 600);
    // Rewrite the trailing validator argument to the default key.
    let n = ix.data.len();
    ix.data[n - 32..].copy_from_slice(&[0u8; 32]);
    let r = env.relayer.insecure_clone();
    assert_zerr!(env.send(&[via_app(ix)], &[&r]), BadParams);
}

#[test]
fn credit_amount_must_be_a_ledger_denomination() {
    let mut env = Env::new();
    let k = Keypair::new();
    assert_zerr!(env.credit([1; 32], 1_234_567, k.pubkey(), [7; 32]), BadDenomination);
    ok!(env.credit([2; 32], 1_000_000, k.pubkey(), [7; 32]));
    ok!(env.credit([3; 32], 10_000_000, k.pubkey(), [7; 32]));
    env.check_invariants();
}
