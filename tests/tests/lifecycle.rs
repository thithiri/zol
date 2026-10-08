//! Configuration, app lifecycle, guardians, pause and freeze semantics.

use solana_keypair::Keypair;
use solana_signer::Signer;
use zol_ledger::state::AppState;
use zol_ledger::SetConfigArgs;
use zol_tests::*;

const POT: [u8; 32] = [0x02; 32];

#[test]
fn only_upgrade_authority_initializes_once() {
    let mut env = Env::bare();
    let mallory = Keypair::new();
    env.svm.airdrop(&mallory.pubkey(), 10_000_000_000).unwrap();
    let pd = solana_pubkey::Pubkey::find_program_address(&[LEDGER.as_ref()], &solana_sdk_ids::bpf_loader_upgradeable::id()).0;
    assert_zerr!(env.initialize_as(&mallory, pd), NotUpgradeAuthority);
    ok!(env.initialize());
    assert!(env.initialize().is_err(), "config is a singleton");
}

#[test]
fn params_are_capped_by_program_constants() {
    let mut env = Env::new();
    let mut p = params();
    p.exit_fee_bps = 101;
    assert_zerr!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }), FeeTooHigh);
    let mut p = params();
    p.network_fee_allowance = 100_001;
    assert_zerr!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }), FeeTooHigh);
    let mut p = params();
    p.activation_delay_secs = 60;
    assert_zerr!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }), BadParams);
    let mut p = params();
    p.refund_grace_secs = 600; // shorter than a forced undelegation can take
    assert_zerr!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }), BadParams);
    let mut p = params();
    p.min_exit = 10_000; // a payout would not cover its own deductions
    assert_zerr!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }), BadParams);
}

#[test]
fn activation_waits_for_delay_and_guardian_can_cancel() {
    let mut env = Env::bare();
    ok!(env.initialize());
    ok!(env.register_test_app());
    ok!(env.set_cap(&TEST_APP, u64::MAX / 4));
    let k = Keypair::new();
    // Pending: no credits.
    assert_zerr!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32]), AppNotActive);
    ok!(env.activate(&TEST_APP));
    assert_eq!(env.app_state().state, AppState::Activating);
    assert_zerr!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32]), AppNotActive);
    assert!(env.activate(&TEST_APP).is_err(), "vault is set once");

    // One guardian cancels inside the delay.
    ok!(env.freeze(1));
    let a = env.app_state();
    assert_eq!((a.state, a.vault_address, a.genesis_head), (AppState::Pending, [0; 43], [0; 32]));

    ok!(env.activate(&TEST_APP));
    env.warp(params().activation_delay_secs - 1);
    assert_zerr!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32]), AppNotActive);
    env.warp(1);
    ok!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32]));
    assert_eq!(env.app_state().state, AppState::Active);
    // Once active the vault can never change.
    assert_zerr!(env.activate(&TEST_APP), BadAppState);
}

#[test]
fn freeze_by_one_unfreeze_by_threshold_never_admin() {
    let mut env = Env::new();
    ok!(env.freeze(0));
    assert_eq!(env.app_state().state, AppState::Frozen);
    let g0 = env.guardians[0].insecure_clone();
    let g1 = env.guardians[1].insecure_clone();
    let admin = env.admin.insecure_clone();
    assert_zerr!(env.unfreeze(&[&g0]), GuardianThreshold);
    assert_zerr!(env.unfreeze(&[&g0, &g0]), GuardianThreshold);
    assert_zerr!(env.unfreeze(&[&g0, &admin]), GuardianThreshold);
    ok!(env.unfreeze(&[&g0, &g1]));
    assert_eq!(env.app_state().state, AppState::Active);
    // A non-guardian cannot freeze.
    let ix = ledger_ix(
        zol_ledger::accounts::FreezeApp { guardian: admin.pubkey(), config: config_pda(), app: app_pda(&TEST_APP) },
        zol_ledger::instruction::FreezeApp {},
    );
    assert_zerr!(env.send(&[ix], &[&admin]), NotGuardian);
}

#[test]
fn frozen_app_blocks_value_entry_but_lets_players_out() {
    let mut env = Env::new();
    let alice = env.player(1_000_000);
    let bob = env.player(1_000_000);
    ok!(env.open_pot(POT, 600));
    ok!(env.buy_in(POT, &alice, 200_000));
    ok!(env.buy_in(POT, &bob, 200_000));
    ok!(env.open_pot([0x03; 32], 600));
    ok!(env.buy_in([0x03; 32], &alice, 100_000));
    let e = env.expected(POT);
    ok!(env.delegate(POT, e, 600));

    ok!(env.freeze(2));
    let k = Keypair::new();
    assert_zerr!(env.credit([9; 32], 1_000_000, k.pubkey(), [7; 32]), AppFrozen);
    assert_zerr!(env.buy_in([0x03; 32], &bob, 1), AppFrozen);
    assert_zerr!(env.open_pot([0x04; 32], 600), AppFrozen);
    let e = env.expected([0x03; 32]);
    assert_zerr!(env.delegate([0x03; 32], e, 600), AppFrozen);
    // Still allowed: void, leave an open pot, return and cash out, request exit.
    ok!(env.void([9; 32]));
    ok!(env.leave_pot([0x03; 32], &alice));
    env.simulate_return(POT, |p| p.delegated = false);
    ok!(env.cash_out(POT, &[alice.pubkey(), bob.pubkey()]));
    ok!(env.request_exit(&alice));
    assert_zerr!(env.settle(&[alice.pubkey()]), AppFrozen);
    env.check_invariants();
}

#[test]
fn pause_blocks_like_freeze() {
    let mut env = Env::new();
    let alice = env.player(1_000_000);
    ok!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: None, paused: Some(true) }));
    let k = Keypair::new();
    assert_zerr!(env.credit([9; 32], 1_000_000, k.pubkey(), [7; 32]), Paused);
    assert_zerr!(env.open_pot(POT, 600), Paused);
    ok!(env.request_exit(&alice));
    assert_zerr!(env.settle(&[alice.pubkey()]), Paused);
    ok!(env.set_config(SetConfigArgs { new_admin: None, attestor: None, params: None, paused: Some(false) }));
    ok!(env.settle(&[alice.pubkey()]));
}

#[test]
fn guardian_set_changes_need_guardian_threshold() {
    let mut env = Env::new();
    let new: Vec<_> = (0..3).map(|_| Keypair::new()).collect();
    let mk = |env: &Env, signers: &[&Keypair]| {
        let mut ix = ledger_ix(
            zol_ledger::accounts::GuardianQuorum { config: config_pda() },
            zol_ledger::instruction::SetGuardians { guardians: new.iter().map(|k| k.pubkey()).collect(), threshold: 2 },
        );
        for s in signers {
            ix.accounts.push(solana_instruction::AccountMeta::new_readonly(s.pubkey(), true));
        }
        let _ = env;
        ix
    };
    let g0 = env.guardians[0].insecure_clone();
    let g2 = env.guardians[2].insecure_clone();
    let admin = env.admin.insecure_clone();
    let r = env.relayer.insecure_clone();
    let ix = mk(&env, &[&admin, &g0]);
    assert_zerr!(env.send(&[ix], &[&r, &admin, &g0]), GuardianThreshold);
    let ix = mk(&env, &[&g0, &g2]);
    ok!(env.send(&[ix], &[&r, &g0, &g2]));
    let c: st::Config = env.account(&config_pda());
    assert_eq!(c.guardians[0], new[0].pubkey());
}

#[test]
fn cap_bounds_credits() {
    let mut env = Env::new();
    ok!(env.set_cap(&TEST_APP, 1_000_000));
    let k = Keypair::new();
    ok!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32])); // net 997_000 fits
    assert_zerr!(env.credit([2; 32], 10_000, k.pubkey(), [7; 32]), OverCap);
}

#[test]
fn register_needs_the_apps_own_signature() {
    let mut env = Env::bare();
    ok!(env.initialize());
    let fake = Keypair::new();
    let ix = ledger_ix(
        zol_ledger::accounts::RegisterApp {
            payer: env.relayer.pubkey(),
            config: config_pda(),
            app_program: TEST_APP,
            app_authority: fake.pubkey(),
            app: app_pda(&TEST_APP),
            system_program: SYSTEM,
        },
        zol_ledger::instruction::RegisterApp {},
    );
    let r = env.relayer.insecure_clone();
    assert_zerr!(env.send(&[ix], &[&r, &fake]), BadAppAuthority);
    ok!(env.register_test_app());
}
