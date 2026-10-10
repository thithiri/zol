//! The `dev-timings` ledger build (target/deploy-dev): short activation and
//! refund-grace floors for local stacks, and never initializable for mainnet.
//! The release build's floors are covered by `lifecycle.rs`.

use solana_signer::Signer;
use zol_ledger::state::AppState;
use zol_tests::*;

fn dev_dir() -> std::path::PathBuf {
    deploy_dir().join("../deploy-dev")
}

fn program_data() -> solana_pubkey::Pubkey {
    solana_pubkey::Pubkey::find_program_address(&[LEDGER.as_ref()], &solana_sdk_ids::bpf_loader_upgradeable::id()).0
}

#[test]
fn dev_build_allows_short_windows() {
    let mut env = Env::bare_from(&dev_dir());
    let mut p = params();
    p.activation_delay_secs = 5;
    p.refund_grace_secs = 10;
    let admin = env.admin.insecure_clone();
    ok!(env.initialize_with(&admin, program_data(), p));
    ok!(env.register_test_app());
    ok!(env.activate(&TEST_APP));
    ok!(env.set_cap(&TEST_APP, 1_000_000_000));
    env.warp(5);
    let k = solana_keypair::Keypair::new();
    ok!(env.credit([1; 32], 1_000_000, k.pubkey(), [7; 32]));
    assert_eq!(env.app_state().state, AppState::Active);
}

#[test]
fn dev_build_refuses_mainnet() {
    let mut env = Env::bare_from(&dev_dir());
    env.network = zol_wire::Network::Mainnet as u8;
    let admin = env.admin.insecure_clone();
    assert_zerr!(env.initialize_with(&admin, program_data(), params()), BadParams);
}

#[test]
fn release_build_rejects_dev_windows() {
    let mut env = Env::bare();
    let mut p = params();
    p.activation_delay_secs = 5;
    let admin = env.admin.insecure_clone();
    assert_zerr!(env.initialize_with(&admin, program_data(), p), BadParams);
    let mut p = params();
    p.refund_grace_secs = 10;
    assert_zerr!(env.initialize_with(&admin, program_data(), p), BadParams);
}
