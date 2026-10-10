//! zol-coinflip on LiteSVM against the real ledger, the real delegation program
//! (`fixtures/dlp.so`) and MagicBlock's VRF program (`fixtures/vrf.so`), with the local
//! ephemeral test queue emptied (`fixtures/vrf-ephemeral-test-queue-empty.json`, made by
//! spikes/magicblock/vrf-queues/clean.py: the npm dump is full of stale requests, so any
//! new request fails with AccountDataTooSmall). It is loaded owned by the VRF program, as
//! the rollup sees it.
//!
//! What LiteSVM cannot run is simulated:
//! - "In the rollup": after `start`, the pot and the table are handed back to their
//!   programs with the same bytes, which is what the ephemeral validator executes on.
//! - The VRF oracle: `land` is sent with the scoped VRF identity marked as signer
//!   and signature verification off, exactly the instruction the oracle's
//!   fulfilment CPIs. Every other test runs with signature checks on.
//! - The magic program: a no-op program sits at its address, so `finish` runs the
//!   ledger's `close_pot` and the table commit up to the scheduling CPI.
//! - The return: the accounts already belong to their programs with the committed
//!   bytes, which is what the delegation program's undelegation restores.
#![allow(clippy::result_large_err)]

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use base64::Engine;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use zol_coinflip::{CoinflipError, Table, TableState, NO_WINNER};
use zol_ledger::state::PotState;
use zol_tests::*;

const COINFLIP: Pubkey = zol_coinflip::ID;
const VRF: Pubkey = solana_pubkey::pubkey!("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz");
const VRF_QUEUE: Pubkey = solana_pubkey::pubkey!("Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT");
/// A real VRF queue (the base-layer test queue) that `flip` must still refuse.
const OTHER_QUEUE: Pubkey = solana_pubkey::pubkey!("GKE6d7iv8kCBrsxr78W3xVdjGLLLJnxsGiuzrsZCGEvb");
const MAGIC_PROGRAM: Pubkey = solana_pubkey::pubkey!("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT: Pubkey = solana_pubkey::pubkey!("MagicContext1111111111111111111111111111111");
const SLOT_HASHES: Pubkey = solana_pubkey::pubkey!("SysvarS1otHashes111111111111111111111111111");
/// One 1,000,000-zatoshi deposit's net credited value at the 30 bps entry fee.
const STAKE: u64 = 997_000;
const POT_ID: [u8; 32] = [0xC0; 32];

fn cf_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id: COINFLIP, accounts: accounts.to_account_metas(None), data: data.data() }
}
fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name)
}
fn cf_app() -> Pubkey {
    app_pda(&COINFLIP)
}
fn cf_authority() -> Pubkey {
    app_authority(&COINFLIP)
}
fn cf_pot() -> Pubkey {
    pot_pda(&cf_app(), &POT_ID)
}
fn cf_table() -> Pubkey {
    zol_coinflip::table_address(&POT_ID)
}
/// `PDA(["identity", callback_program], vrf)`: the only signer `land` accepts.
fn scoped_vrf_identity() -> Pubkey {
    pda(&[b"identity", COINFLIP.as_ref()], &VRF)
}
fn rent_vault() -> Pubkey {
    pda(&[zol_coinflip::RENT_VAULT_SEED], &COINFLIP)
}
fn cf_err(e: CoinflipError) -> u32 {
    6000 + e as u32
}

struct Cf {
    env: Env,
}

impl Cf {
    /// Ledger initialized; coinflip registered through its own `register`, activated
    /// and past its delay, with a cap.
    fn new() -> Self {
        let mut env = Env::bare();
        let so = std::fs::read(deploy_dir().join("zol_coinflip.so")).expect("run scripts/build-sbf.sh");
        env.svm.add_program(COINFLIP, &so).unwrap();
        env.svm.add_program(VRF, &std::fs::read(fixture("vrf.so")).unwrap()).unwrap();
        env.svm.add_program(MAGIC_PROGRAM, &std::fs::read(fixture("noop.so")).unwrap()).unwrap();
        for (key, file) in [(VRF_QUEUE, "vrf-ephemeral-test-queue-empty.json"), (OTHER_QUEUE, "vrf-test-queue.json")] {
            let q: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(fixture(file)).unwrap()).unwrap();
            let a = &q["account"];
            env.svm
                .set_account(
                    key,
                    Account {
                        lamports: a["lamports"].as_u64().unwrap(),
                        data: base64::engine::general_purpose::STANDARD.decode(a["data"][0].as_str().unwrap()).unwrap(),
                        owner: VRF,
                        executable: false,
                        rent_epoch: 0,
                    },
                )
                .unwrap();
        }
        ok!(env.initialize());
        let mut cf = Cf { env };
        ok!(cf.register());
        ok!(cf.env.activate(&COINFLIP));
        cf.env.warp(params().activation_delay_secs);
        ok!(cf.env.set_cap(&COINFLIP, u64::MAX / 4));
        cf
    }

    fn relayer(&self) -> Keypair {
        self.env.relayer.insecure_clone()
    }

    fn register(&mut self) -> TxResult {
        let ix = cf_ix(
            zol_coinflip::accounts::Register {
                payer: self.env.relayer.pubkey(),
                this_program: COINFLIP,
                app_authority: cf_authority(),
                config: config_pda(),
                app: cf_app(),
                ledger_program: LEDGER,
                system_program: SYSTEM,
            },
            zol_coinflip::instruction::Register {},
        );
        let r = self.relayer();
        self.env.send(&[ix], &[&r])
    }

    /// A player with a credited deposit in the coinflip app.
    fn player(&mut self, amount: u64) -> Keypair {
        let k = Keypair::new();
        self.env.svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
        let args = self.env.credit_args(rand::random(), amount, k.pubkey(), [7; 32], 0);
        let digest = Env::credit_digest(&args, &COINFLIP);
        let ix = ledger_ix(
            zol_ledger::accounts::CreditDeposit {
                relayer: self.env.relayer.pubkey(),
                config: config_pda(),
                app: cf_app(),
                deposit: deposit_pda(&args.deposit_id),
                balance: balance_pda(&cf_app(), &k.pubkey()),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::CreditDeposit { args },
        );
        let r = self.relayer();
        let att = self.env.attestor.clone();
        ok!(self.env.send(&[ed25519_ix(&att, &digest), ix], &[&r]));
        k
    }

    fn create_table(&mut self, stake: u64) -> TxResult {
        self.create_table_for(stake, VALIDATOR)
    }

    fn create_table_for(&mut self, stake: u64, validator: Pubkey) -> TxResult {
        self.create_table_at(POT_ID, stake, validator)
    }

    fn create_table_at(&mut self, pot_id: [u8; 32], stake: u64, validator: Pubkey) -> TxResult {
        let pot = pot_pda(&cf_app(), &pot_id);
        let ix = cf_ix(
            zol_coinflip::accounts::CreateTable {
                payer: self.env.relayer.pubkey(),
                table: zol_coinflip::table_address(&pot_id),
                app_authority: cf_authority(),
                config: config_pda(),
                app: cf_app(),
                pot,
                receipt: receipt_pda(&pot),
                ledger_program: LEDGER,
                system_program: SYSTEM,
            },
            zol_coinflip::instruction::CreateTable { pot_id, stake, open_secs: 600, validator },
        );
        let r = self.relayer();
        self.env.send(&[ix], &[&r])
    }

    fn join_ix(&self, session: &Pubkey) -> Instruction {
        let pot = cf_pot();
        cf_ix(
            zol_coinflip::accounts::Join {
                session_key: *session,
                table: cf_table(),
                app_authority: cf_authority(),
                config: config_pda(),
                app: cf_app(),
                balance: balance_pda(&cf_app(), session),
                pot,
                receipt: receipt_pda(&pot),
                ledger_program: LEDGER,
            },
            zol_coinflip::instruction::Join {},
        )
    }

    fn join(&mut self, p: &Keypair) -> TxResult {
        let ix = self.join_ix(&p.pubkey());
        let r = self.relayer();
        self.env.send(&[ix], &[&r, p])
    }

    fn start(&mut self) -> TxResult {
        let pot = cf_pot();
        let table = cf_table();
        let tag = |t: &[u8], k: &Pubkey| pda(&[t, k.as_ref()], &DLP);
        let ix = cf_ix(
            zol_coinflip::accounts::Start {
                payer: self.env.relayer.pubkey(),
                buffer_table: pda(&[b"buffer", table.as_ref()], &COINFLIP),
                delegation_record_table: tag(b"delegation", &table),
                delegation_metadata_table: tag(b"delegation-metadata", &table),
                table,
                app_authority: cf_authority(),
                config: config_pda(),
                app: cf_app(),
                receipt: receipt_pda(&pot),
                pot,
                ledger_buffer_pot: pda(&[b"buffer", pot.as_ref()], &LEDGER),
                ledger_delegation_record_pot: tag(b"delegation", &pot),
                ledger_delegation_metadata_pot: tag(b"delegation-metadata", &pot),
                ledger_program: LEDGER,
                rent_vault: rent_vault(),
                owner_program: COINFLIP,
                delegation_program: DLP,
                system_program: SYSTEM,
            },
            zol_coinflip::instruction::Start { session_secs: 600 },
        );
        let budget = Instruction {
            program_id: solana_pubkey::pubkey!("ComputeBudget111111111111111111111111111111"),
            accounts: vec![],
            data: [vec![2u8], 400_000u32.to_le_bytes().to_vec()].concat(),
        };
        let r = self.relayer();
        self.env.send(&[budget, ix], &[&r])
    }

    /// What the rollup sees after `start`: the same bytes, owned by their programs.
    fn enter_rollup(&mut self) {
        for (k, owner) in [(cf_pot(), LEDGER), (cf_table(), COINFLIP)] {
            let mut a = self.env.raw(&k).unwrap();
            assert_eq!(a.owner, DLP, "{k} should be delegated");
            a.owner = owner;
            self.env.svm.set_account(k, a).unwrap();
        }
    }

    fn flip(&mut self) -> TxResult {
        self.flip_with(VRF_QUEUE)
    }

    fn flip_with(&mut self, queue: Pubkey) -> TxResult {
        let ix = cf_ix(
            zol_coinflip::accounts::Flip {
                payer: self.env.relayer.pubkey(),
                table: cf_table(),
                oracle_queue: queue,
                program_identity: pda(&[b"identity"], &COINFLIP),
                vrf_program: VRF,
                slot_hashes: SLOT_HASHES,
                system_program: SYSTEM,
            },
            zol_coinflip::instruction::Flip { caller_seed: [9; 32] },
        );
        let r = self.relayer();
        self.env.send(&[ix], &[&r])
    }

    fn land_ix(&self, identity: Pubkey, randomness: [u8; 32]) -> Instruction {
        cf_ix(
            zol_coinflip::accounts::Land {
                vrf_program_identity: identity,
                table: cf_table(),
                pot: cf_pot(),
                app_authority: cf_authority(),
                ledger_program: LEDGER,
            },
            zol_coinflip::instruction::Land { randomness },
        )
    }

    /// The oracle's fulfilment: `land` signed by the scoped VRF identity PDA. Only
    /// the VRF program can produce that signature, so signature checks are switched
    /// off for this one transaction.
    fn vrf_callback(&mut self, randomness: [u8; 32]) -> TxResult {
        let identity = scoped_vrf_identity();
        let ix = self.land_ix(identity, randomness);
        self.send_unverified(&[ix])
    }

    fn send_unverified(&mut self, ixs: &[Instruction]) -> TxResult {
        let svm = std::mem::take(&mut self.env.svm);
        self.env.svm = svm.with_sigverify(false);
        let msg = Message::new(ixs, Some(&self.env.relayer.pubkey()));
        let mut tx = Transaction::new_unsigned(msg);
        tx.signatures = vec![Default::default(); tx.message.header.num_required_signatures as usize];
        tx.message.recent_blockhash = self.env.svm.latest_blockhash();
        let r = self.env.svm.send_transaction(tx);
        self.env.svm.expire_blockhash();
        let svm = std::mem::take(&mut self.env.svm);
        self.env.svm = svm.with_sigverify(true);
        r
    }

    fn finish(&mut self) -> TxResult {
        let ix = cf_ix(
            zol_coinflip::accounts::Finish {
                payer: self.env.relayer.pubkey(),
                table: cf_table(),
                pot: cf_pot(),
                app_authority: cf_authority(),
                ledger_program: LEDGER,
                magic_program: MAGIC_PROGRAM,
                magic_context: MAGIC_CONTEXT,
            },
            zol_coinflip::instruction::Finish {},
        );
        let r = self.relayer();
        self.env.send(&[ix], &[&r])
    }

    fn cash_out(&mut self) -> TxResult {
        let pot = cf_pot();
        let r: st::PotReceipt = self.env.account(&receipt_pda(&pot));
        let mut ix = ledger_ix(
            zol_ledger::accounts::PotOutcome { config: config_pda(), app: cf_app(), receipt: receipt_pda(&pot), pot },
            zol_ledger::instruction::CashOut {},
        );
        for p in &r.players[..r.seat_count as usize] {
            ix.accounts.push(AccountMeta::new(balance_pda(&cf_app(), p), false));
        }
        let relayer = self.relayer();
        self.env.send(&[ix], &[&relayer])
    }

    fn close_table_ix(&self, creator: Pubkey) -> Instruction {
        cf_ix(
            zol_coinflip::accounts::CloseTable { table: cf_table(), receipt: receipt_pda(&cf_pot()), creator },
            zol_coinflip::instruction::CloseTable {},
        )
    }

    /// Anyone may close: a stranger pays the fee, the rent goes to the creator.
    fn close_table(&mut self) -> TxResult {
        let ix = self.close_table_ix(self.env.relayer.pubkey());
        let stranger = Keypair::new();
        self.env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
        self.env.send(&[ix], &[&stranger])
    }

    fn rescue_ix(&self) -> Instruction {
        let t = cf_table();
        let tag = |x: &[u8]| pda(&[x, t.as_ref()], &DLP);
        let validator = self.table_any().validator;
        cf_ix(
            zol_coinflip::accounts::RescueTable {
                table: t,
                receipt: receipt_pda(&cf_pot()),
                rent_vault: rent_vault(),
                undelegation_request: tag(b"undelegation-request"),
                delegation_record: tag(b"delegation"),
                delegation_metadata: tag(b"delegation-metadata"),
                commit_state: tag(b"state-diff"),
                commit_record: tag(b"commit-state-record"),
                validator,
                this_program: COINFLIP,
                delegation_program: DLP,
                system_program: SYSTEM,
            },
            zol_coinflip::instruction::RescueTable {},
        )
    }

    /// Anyone: a stranger pays the fee.
    fn rescue(&mut self) -> TxResult {
        let ix = self.rescue_ix();
        let stranger = Keypair::new();
        self.env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
        self.env.send(&[ix], &[&stranger])
    }

    /// The table's bytes whoever owns it (the delegation program keeps the last committed copy).
    fn table_any(&self) -> Table {
        let a = self.env.raw(&cf_table()).unwrap();
        Table::try_deserialize(&mut &a.data[..]).unwrap()
    }

    fn refund(&mut self) -> TxResult {
        let pot = cf_pot();
        let r: st::PotReceipt = self.env.account(&receipt_pda(&pot));
        let mut ix = ledger_ix(
            zol_ledger::accounts::PotOutcome { config: config_pda(), app: cf_app(), receipt: receipt_pda(&pot), pot },
            zol_ledger::instruction::RefundPot {},
        );
        for p in &r.players[..r.seat_count as usize] {
            ix.accounts.push(AccountMeta::new(balance_pda(&cf_app(), p), false));
        }
        let relayer = self.relayer();
        self.env.send(&[ix], &[&relayer])
    }

    fn lamports(&self, k: &Pubkey) -> u64 {
        self.env.raw(k).map(|a| a.lamports).unwrap_or(0)
    }

    fn table(&self) -> Table {
        self.env.account(&cf_table())
    }
    fn pot(&self) -> st::Pot {
        let a = self.env.raw(&cf_pot()).unwrap();
        st::Pot::try_deserialize(&mut &a.data[..]).unwrap()
    }
    fn receipt(&self) -> st::PotReceipt {
        self.env.account(&receipt_pda(&cf_pot()))
    }
    fn balance(&self, p: &Pubkey) -> u64 {
        self.env.account::<st::Balance>(&balance_pda(&cf_app(), p)).amount
    }

    /// Two players seated and the table started.
    fn started() -> (Self, Keypair, Keypair) {
        let mut cf = Cf::new();
        let a = cf.player(1_000_000);
        let b = cf.player(1_000_000);
        ok!(cf.create_table(STAKE));
        ok!(cf.join(&a));
        ok!(cf.join(&b));
        ok!(cf.start());
        (cf, a, b)
    }
}

fn assert_cf_err(r: &TxResult, e: CoinflipError) {
    assert_eq!(err_code(r), Some(cf_err(e)), "{:?}", r.as_ref().map(|_| ()).map_err(|f| f.meta.logs.clone()));
}

fn randomness_for(seat: u8) -> [u8; 32] {
    (0u8..=255).map(|b| [b; 32]).find(|r| zol_coinflip::outcome(r) == seat).unwrap()
}

// ------------------------------------------------------------------ happy path

#[test]
fn full_game_create_join_start_flip_land_finish_cash_out() {
    let mut cf = Cf::new();
    let alice = cf.player(1_000_000);
    let bob = cf.player(1_000_000);
    assert_eq!(cf.balance(&alice.pubkey()), 997_000);

    ok!(cf.create_table(STAKE));
    let t = cf.table();
    assert_eq!((t.state, t.stake, t.pot, t.winner, t.validator), (TableState::Open, STAKE, cf_pot(), NO_WINNER, VALIDATOR));
    assert_eq!(cf.receipt().state, PotState::Open);

    ok!(cf.join(&alice));
    ok!(cf.join(&bob));
    let r = cf.receipt();
    assert_eq!(r.seat_count, 2);
    assert_eq!((r.players[0], r.players[1]), (alice.pubkey(), bob.pubkey()));
    assert_eq!((r.buy_ins[0], r.buy_ins[1], r.total), (STAKE, STAKE, 2 * STAKE));
    assert_eq!(cf.balance(&alice.pubkey()), 0, "one deposit's net value is exactly one stake");

    ok!(cf.start());
    assert_eq!(cf.env.raw(&cf_pot()).unwrap().owner, DLP, "pot delegated through the ledger");
    assert_eq!(cf.env.raw(&cf_table()).unwrap().owner, DLP, "table delegated by the app");
    let r = cf.receipt();
    assert_eq!((r.state, r.validator), (PotState::Delegated, VALIDATOR));
    cf.env.check_invariants_for(&COINFLIP);

    cf.enter_rollup();
    assert_eq!(cf.table().state, TableState::Started);
    ok!(cf.flip());
    assert_eq!(cf.table().state, TableState::Flipping);

    // Bob (seat 1) wins.
    ok!(cf.vrf_callback(randomness_for(1)));
    let t = cf.table();
    assert_eq!((t.state, t.winner), (TableState::Landed, 1));
    assert_eq!(&cf.pot().amounts[..2], &[0, 2 * STAKE]);

    ok!(cf.finish());
    assert_eq!(cf.table().state, TableState::Finished);
    assert!(!cf.pot().delegated, "close_pot marked the pot as returning");

    // Return: accounts already belong to their programs with the committed bytes.
    ok!(cf.cash_out());
    assert_eq!(cf.receipt().state, PotState::CashedOut);
    assert_eq!(cf.balance(&alice.pubkey()), 0);
    assert_eq!(cf.balance(&bob.pubkey()), 2 * STAKE, "winner credited with the whole pot");
    cf.env.check_invariants_for(&COINFLIP);
}

#[test]
fn seat_zero_can_win() {
    let (mut cf, alice, bob) = Cf::started();
    cf.enter_rollup();
    ok!(cf.flip());
    ok!(cf.vrf_callback(randomness_for(0)));
    assert_eq!(&cf.pot().amounts[..2], &[2 * STAKE, 0]);
    ok!(cf.finish());
    ok!(cf.cash_out());
    assert_eq!((cf.balance(&alice.pubkey()), cf.balance(&bob.pubkey())), (2 * STAKE, 0));
}

// ------------------------------------------------------------------ adversarial

#[test]
fn non_player_cannot_join_with_someone_elses_session_key() {
    let mut cf = Cf::new();
    let alice = cf.player(1_000_000);
    let mallory = Keypair::new();
    cf.env.svm.airdrop(&mallory.pubkey(), 1_000_000_000).unwrap();
    ok!(cf.create_table(STAKE));

    // Alice's session key named but not signing: Anchor's Signer check refuses it.
    let mut ix = cf.join_ix(&alice.pubkey());
    ix.accounts[0].is_signer = false;
    let r = cf.env.send(&[ix], &[&mallory]);
    assert_eq!(err_code(&r), Some(anchor_lang::error::ErrorCode::AccountNotSigner as u32));
    // Mallory signs with her own key but points at Alice's balance: the ledger's seeds refuse it.
    let mut ix = cf.join_ix(&mallory.pubkey());
    ix.accounts[5].pubkey = balance_pda(&cf_app(), &alice.pubkey());
    assert!(cf.env.send(&[ix], &[&mallory]).is_err());
    assert_eq!(cf.receipt().seat_count, 0);
    assert_eq!(cf.balance(&alice.pubkey()), 997_000);
}

#[test]
fn third_player_and_double_join_are_refused() {
    let mut cf = Cf::new();
    let a = cf.player(1_000_000);
    let b = cf.player(1_000_000);
    let c = cf.player(1_000_000);
    ok!(cf.create_table(STAKE));
    ok!(cf.join(&a));
    assert_cf_err(&cf.join(&a), CoinflipError::AlreadySeated);
    ok!(cf.join(&b));
    assert_cf_err(&cf.join(&c), CoinflipError::TableFull);
    assert_eq!(cf.receipt().seat_count, 2);
    assert_eq!(cf.balance(&c.pubkey()), 997_000);
}

#[test]
fn table_must_name_a_validator_and_start_uses_it() {
    let mut cf = Cf::new();
    assert_cf_err(&cf.create_table_for(STAKE, Pubkey::default()), CoinflipError::BadValidator);
    // Whoever calls `start`, the pot goes to the validator the table named at creation.
    let other = Keypair::new().pubkey();
    ok!(cf.create_table_for(STAKE, other));
    let a = cf.player(1_000_000);
    let b = cf.player(1_000_000);
    ok!(cf.join(&a));
    ok!(cf.join(&b));
    ok!(cf.start());
    assert_eq!(cf.receipt().validator, other);
}

#[test]
fn start_needs_two_players_and_runs_once() {
    let mut cf = Cf::new();
    let a = cf.player(1_000_000);
    ok!(cf.create_table(STAKE));
    ok!(cf.join(&a));
    assert_cf_err(&cf.start(), CoinflipError::NotFull);
    let b = cf.player(1_000_000);
    ok!(cf.join(&b));
    ok!(cf.start());
    // The table now belongs to the delegation program.
    assert!(cf.start().is_err());
}

#[test]
fn a_player_who_leaves_before_start_is_not_in_the_game() {
    let mut cf = Cf::new();
    let a = cf.player(1_000_000);
    let b = cf.player(1_000_000);
    let c = cf.player(1_000_000);
    ok!(cf.create_table(STAKE));
    ok!(cf.join(&a));
    ok!(cf.join(&b));
    // The ledger lets a player leave an open pot without the app.
    let pot = cf_pot();
    let ix = ledger_ix(
        zol_ledger::accounts::LeavePot { session_key: b.pubkey(), balance: balance_pda(&cf_app(), &b.pubkey()), pot, receipt: receipt_pda(&pot) },
        zol_ledger::instruction::LeavePot {},
    );
    let r = cf.relayer();
    ok!(cf.env.send(&[ix], &[&r, &b]));
    assert_cf_err(&cf.start(), CoinflipError::NotFull);
    ok!(cf.join(&c));
    ok!(cf.start());
    let r = cf.receipt();
    assert_eq!((r.players[0], r.players[1]), (a.pubkey(), c.pubkey()), "the receipt, not the app, says who plays");
}

#[test]
fn land_only_through_the_vrf_callback() {
    let (mut cf, _a, _b) = Cf::started();
    cf.enter_rollup();
    ok!(cf.flip());

    // A player signs `land` as if they were the VRF identity: wrong address.
    let mallory = Keypair::new();
    cf.env.svm.airdrop(&mallory.pubkey(), 1_000_000_000).unwrap();
    let ix = cf.land_ix(mallory.pubkey(), randomness_for(0));
    let r = cf.env.send(&[ix], &[&mallory]);
    assert_eq!(err_code(&r), Some(anchor_lang::error::ErrorCode::ConstraintAddress as u32));
    // The real identity named but not signing.
    let identity = scoped_vrf_identity();
    let mut ix = cf.land_ix(identity, randomness_for(0));
    ix.accounts[0].is_signer = false;
    let r = cf.env.send(&[ix], &[&mallory]);
    assert_eq!(err_code(&r), Some(anchor_lang::error::ErrorCode::AccountNotSigner as u32));
    // The legacy global identity is not accepted either.
    let legacy = solana_pubkey::pubkey!("9irBy75QS2BN81FUgXuHcjqceJJRuc9oDkAe8TKVvvAw");
    let r = cf.send_unverified(&[cf.land_ix(legacy, randomness_for(0))]);
    assert_eq!(err_code(&r), Some(anchor_lang::error::ErrorCode::ConstraintAddress as u32));
    assert_eq!(cf.table().state, TableState::Flipping);

    ok!(cf.vrf_callback(randomness_for(1)));
    // A second callback (replay) is refused: the coin has landed.
    assert_cf_err(&cf.vrf_callback(randomness_for(0)), CoinflipError::BadTableState);
    assert_eq!(&cf.pot().amounts[..2], &[0, 2 * STAKE]);
}

#[test]
fn flip_refuses_an_unpinned_oracle_queue() {
    let (mut cf, _a, _b) = Cf::started();
    cf.enter_rollup();
    assert_cf_err(&cf.flip_with(OTHER_QUEUE), CoinflipError::BadQueue);
    assert_cf_err(&cf.flip_with(Keypair::new().pubkey()), CoinflipError::BadQueue);
    assert_eq!(cf.table().state, TableState::Started);
    ok!(cf.flip());
}

#[test]
fn land_before_flip_and_finish_before_land_are_refused() {
    let (mut cf, _a, _b) = Cf::started();
    cf.enter_rollup();
    assert_cf_err(&cf.vrf_callback(randomness_for(0)), CoinflipError::BadTableState);
    assert_cf_err(&cf.finish(), CoinflipError::BadTableState);
    ok!(cf.flip());
    assert_cf_err(&cf.flip(), CoinflipError::BadTableState);
    assert_cf_err(&cf.finish(), CoinflipError::BadTableState);
}

#[test]
fn double_finish_fails() {
    let (mut cf, _a, _b) = Cf::started();
    cf.enter_rollup();
    ok!(cf.flip());
    ok!(cf.vrf_callback(randomness_for(0)));
    ok!(cf.finish());
    assert_cf_err(&cf.finish(), CoinflipError::BadTableState);
    ok!(cf.cash_out());
    // And the ledger refuses a second cash-out.
    assert!(cf.cash_out().is_err());
}

#[test]
fn cash_out_waits_for_the_pot() {
    let (mut cf, _a, _b) = Cf::started();
    // Still delegated on Solana: the ledger refuses.
    assert_eq!(err_code(&cf.cash_out()), Some(zerr(zol_ledger::error::ZolError::PotNotReturned)));
}

// ------------------------------------------------------------------ close_table

#[test]
fn close_after_cash_out_returns_rent_to_the_creator() {
    let (mut cf, _a, _b) = Cf::started();
    cf.enter_rollup();
    ok!(cf.flip());
    ok!(cf.vrf_callback(randomness_for(0)));
    ok!(cf.finish());
    assert_eq!(cf.table().creator, cf.env.relayer.pubkey());
    assert_cf_err(&cf.close_table(), CoinflipError::PotLive); // finished, but not cashed out yet
    ok!(cf.cash_out());
    let rent = cf.lamports(&cf_table());
    let before = cf.lamports(&cf.env.relayer.pubkey());
    ok!(cf.close_table());
    assert_eq!(cf.lamports(&cf.env.relayer.pubkey()), before + rent, "all of the table's lamports, to the creator");
    assert!(cf.env.raw(&cf_table()).is_none_or(|a| a.lamports == 0 && a.data.is_empty()));
    // A double close fails: the table is gone.
    assert!(cf.close_table().is_err());
}

#[test]
fn close_a_refunded_table_that_never_started() {
    let mut cf = Cf::new();
    let a = cf.player(1_000_000);
    ok!(cf.create_table(STAKE));
    ok!(cf.join(&a));
    assert_cf_err(&cf.close_table(), CoinflipError::PotLive);
    cf.env.warp(601); // past the open deadline
    ok!(cf.refund());
    assert_eq!(cf.receipt().state, PotState::Refunded);
    assert_eq!(cf.balance(&a.pubkey()), STAKE);
    let rent = cf.lamports(&cf_table());
    let before = cf.lamports(&cf.env.relayer.pubkey());
    ok!(cf.close_table());
    assert_eq!(cf.lamports(&cf.env.relayer.pubkey()), before + rent);
}

#[test]
fn close_refused_while_the_pot_is_live_or_delegated() {
    let (mut cf, _a, _b) = Cf::started();
    // Delegated: the table belongs to the delegation program.
    assert_eq!(err_code(&cf.close_table()), Some(anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram as u32));
    // Back with its program but the receipt is still Delegated.
    cf.enter_rollup();
    assert_cf_err(&cf.close_table(), CoinflipError::PotLive);
}

#[test]
fn close_rent_goes_only_to_the_creator() {
    let mut cf = Cf::new();
    ok!(cf.create_table(STAKE));
    cf.env.warp(601);
    ok!(cf.refund()); // nobody joined: an empty refund
    let mallory = Keypair::new();
    cf.env.svm.airdrop(&mallory.pubkey(), 1_000_000_000).unwrap();
    let ix = cf.close_table_ix(mallory.pubkey());
    assert_cf_err(&cf.env.send(&[ix], &[&mallory]), CoinflipError::BadAccount);
    // A receipt that is not this table's is refused too.
    let mut ix = cf.close_table_ix(cf.env.relayer.pubkey());
    ix.accounts[1].pubkey = config_pda();
    assert_cf_err(&cf.env.send(&[ix], &[&mallory]), CoinflipError::BadAccount);
    ok!(cf.close_table());
}

// ------------------------------------------------------------------ stakes

#[test]
fn stake_must_be_a_denominations_net_value() {
    let mut cf = Cf::new();
    for bad in [500_000, 1_000_000, 996_999, 10_000_000, 1] {
        assert_cf_err(&cf.create_table(bad), CoinflipError::BadStake);
    }
    // Both denominations at the configured 30 bps: 997,000 and 9,970,000.
    assert_eq!(zol_coinflip::allowed_stakes(&params()), vec![997_000, 9_970_000]);
    ok!(cf.create_table_at([1; 32], 997_000, VALIDATOR));
    ok!(cf.create_table_at([2; 32], 9_970_000, VALIDATOR));
}

#[test]
fn stakes_follow_the_ledgers_entry_fee() {
    let mut cf = Cf::new();
    let mut p = params();
    p.entry_fee_bps = 50;
    ok!(cf.env.set_config(zol_ledger::SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }));
    assert_cf_err(&cf.create_table_at([1; 32], 997_000, VALIDATOR), CoinflipError::BadStake);
    ok!(cf.create_table_at([1; 32], 995_000, VALIDATOR));
    ok!(cf.create_table_at([2; 32], 9_950_000, VALIDATOR));
}

#[test]
fn create_table_checks_the_config_account() {
    let mut cf = Cf::new();
    let pot = cf_pot();
    // A look-alike Config at another address, or any other account, is refused.
    let mut ix = cf_ix(
        zol_coinflip::accounts::CreateTable {
            payer: cf.env.relayer.pubkey(),
            table: cf_table(),
            app_authority: cf_authority(),
            config: cf_app(),
            app: cf_app(),
            pot,
            receipt: receipt_pda(&pot),
            ledger_program: LEDGER,
            system_program: SYSTEM,
        },
        zol_coinflip::instruction::CreateTable { pot_id: POT_ID, stake: STAKE, open_secs: 600, validator: VALIDATOR },
    );
    let r = cf.relayer();
    assert_cf_err(&cf.env.send(&[ix.clone()], &[&r]), CoinflipError::BadAccount);
    ix.accounts[3].pubkey = Keypair::new().pubkey();
    assert!(cf.env.send(&[ix], &[&r]).is_err());
}

// ------------------------------------------------------------------ stakes from the ledger

#[test]
fn stakes_follow_the_ledgers_denominations() {
    let mut cf = Cf::new();
    let mut p = params();
    p.denominations = [2_000_000, 0, 50_000_000, 0];
    ok!(cf.env.set_config(zol_ledger::SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None }));
    assert_eq!(zol_coinflip::allowed_stakes(&p), vec![1_994_000, 49_850_000]);
    assert_cf_err(&cf.create_table_at([1; 32], 997_000, VALIDATOR), CoinflipError::BadStake);
    ok!(cf.create_table_at([1; 32], 1_994_000, VALIDATOR));
    ok!(cf.create_table_at([2; 32], 49_850_000, VALIDATOR));
}

// ------------------------------------------------------------------ rescue_table

/// A started table whose rollup never answers: past the session deadline and the
/// refund grace, the ledger refunds the pot while pot and table are still delegated.
fn stuck_and_refunded() -> (Cf, Keypair, Keypair) {
    let (mut cf, a, b) = Cf::started();
    cf.env.warp(600 + params().refund_grace_secs + 1);
    ok!(cf.refund());
    assert_eq!(cf.receipt().state, PotState::Refunded);
    assert_eq!((cf.balance(&a.pubkey()), cf.balance(&b.pubkey())), (STAKE, STAKE), "buy-ins back");
    assert_eq!(cf.env.raw(&cf_table()).unwrap().owner, DLP, "the table is still out");
    (cf, a, b)
}

#[test]
fn start_delegates_the_table_with_the_rent_vault_as_rent_payer() {
    let (cf, _a, _b) = Cf::started();
    let meta = cf.env.raw(&pda(&[b"delegation-metadata", cf_table().as_ref()], &DLP)).unwrap();
    // DelegationMetadata ends with the rent payer.
    assert_eq!(&meta.data[meta.data.len() - 32..], rent_vault().as_ref());
    assert_eq!(cf.env.raw(&rent_vault()).unwrap().owner, SYSTEM, "the vault holds lamports only");
    assert_eq!(cf.table_any().state, TableState::Started, "the Solana copy keeps the committed bytes");
}

#[test]
fn rescue_a_stuck_table_then_close_it() {
    let (mut cf, _a, _b) = stuck_and_refunded();
    assert_eq!(err_code(&cf.close_table()), Some(anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram as u32));
    // 1. Request undelegation (ix 26), signed by the program for the table and the vault.
    ok!(cf.rescue());
    let req = pda(&[b"undelegation-request", cf_table().as_ref()], &DLP);
    assert!(cf.env.raw(&req).is_some_and(|a| a.lamports > 0));
    // 2. Before the timeout the delegation program refuses the rollback.
    assert!(cf.rescue().is_err());
    // 3. After 9,000 slots: rollback (ix 27) brings the table home with its committed bytes.
    let slot = cf.env.svm.get_sysvar::<solana_clock::Clock>().slot;
    cf.env.svm.warp_to_slot(slot + 9_001);
    let vault_before = cf.lamports(&rent_vault());
    ok!(cf.rescue());
    assert_eq!(cf.env.raw(&cf_table()).unwrap().owner, COINFLIP);
    let t = cf.table();
    assert_eq!((t.state, t.pot), (TableState::Started, cf_pot()));
    assert!(cf.lamports(&rent_vault()) > vault_before, "delegation deposit refunded to the vault");
    // 4. Rent back to the creator.
    let rent = cf.lamports(&cf_table());
    let before = cf.lamports(&cf.env.relayer.pubkey());
    ok!(cf.close_table());
    assert_eq!(cf.lamports(&cf.env.relayer.pubkey()), before + rent);
}

#[test]
fn rescue_refused_while_the_game_is_live() {
    let (mut cf, _a, _b) = Cf::started();
    assert_cf_err(&cf.rescue(), CoinflipError::NotRefunded);
    // Even past the session deadline, until the ledger has actually refunded the pot.
    cf.env.warp(600 + params().refund_grace_secs + 1);
    assert_cf_err(&cf.rescue(), CoinflipError::NotRefunded);
}

#[test]
fn rescue_refused_for_a_table_that_is_not_delegated() {
    let mut cf = Cf::new();
    ok!(cf.create_table(STAKE));
    cf.env.warp(601);
    ok!(cf.refund());
    assert_cf_err(&cf.rescue(), CoinflipError::NotDelegated);
}

#[test]
fn rescue_checks_the_receipt_and_validator_accounts() {
    let (mut cf, _a, _b) = stuck_and_refunded();
    let stranger = Keypair::new();
    cf.env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let mut ix = cf.rescue_ix();
    ix.accounts[1].pubkey = config_pda(); // not this table's receipt
    assert_cf_err(&cf.env.send(&[ix], &[&stranger]), CoinflipError::BadAccount);
    let mut ix = cf.rescue_ix();
    ix.accounts[8].pubkey = stranger.pubkey(); // not the table's validator
    assert_cf_err(&cf.env.send(&[ix], &[&stranger]), CoinflipError::BadAccount);
}
