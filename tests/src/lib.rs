//! ZOL ledger test harness on LiteSVM.
#![allow(clippy::result_large_err)]
//!
//! Loads the SBF builds from `target/deploy` (run `scripts/build-sbf.sh` first),
//! the MagicBlock delegation program dumped from devnet (`fixtures/dlp.so`), and
//! drives the ledger directly and through `zol-test-app`, which signs as an app.

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use ed25519_dalek::{Signer as _, SigningKey};
use litesvm::types::{FailedTransactionMetadata, TransactionMetadata};
use litesvm::LiteSVM;
use solana_account::{Account, ReadableAccount};
use solana_clock::Clock;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use zol_ledger::state::{seeds, Params, SeatStake};
use zol_wire::digest::{paid_digest, void_digest, Sha256};
use zol_wire::CreditAttestation;

pub use zol_ledger::state as st;

pub const LEDGER: Pubkey = zol_ledger::ID;
pub const TEST_APP: Pubkey = zol_test_app::ID;
pub const DLP: Pubkey = solana_pubkey::pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
pub const IX_SYSVAR: Pubkey = solana_pubkey::pubkey!("Sysvar1nstructions1111111111111111111111111");
pub const SYSTEM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");
pub const NETWORK: u8 = zol_wire::Network::Testnet as u8;
pub const T0: i64 = 1_800_000_000;
/// Rollup validator identity pots are delegated to in tests.
pub const VALIDATOR: Pubkey = solana_pubkey::pubkey!("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");

pub fn params() -> Params {
    Params {
        entry_fee_bps: 30,
        exit_fee_bps: 30,
        network_fee_allowance: 10_000,
        min_exit: 100_000,
        max_seats: 8,
        max_open_secs: 3_600,
        min_session_secs: 60,
        max_session_secs: 7_200,
        refund_grace_secs: 3_900,
        activation_delay_secs: 3_600,
        registration_bond: 0,
    }
}

pub type TxResult = Result<TransactionMetadata, FailedTransactionMetadata>;

pub struct Env {
    pub svm: LiteSVM,
    pub admin: Keypair,
    pub relayer: Keypair,
    pub attestor: SigningKey,
    pub guardians: Vec<Keypair>,
    pub now: i64,
    /// Zcash network the ledger config binds digests to.
    pub network: u8,
}

pub fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

pub fn config_pda() -> Pubkey {
    pda(&[seeds::CONFIG], &LEDGER)
}
pub fn app_authority(app_program: &Pubkey) -> Pubkey {
    pda(&[seeds::APP_AUTHORITY], app_program)
}
pub fn app_pda(app_program: &Pubkey) -> Pubkey {
    pda(&[seeds::APP, app_program.as_ref()], &LEDGER)
}
pub fn deposit_pda(id: &[u8; 32]) -> Pubkey {
    pda(&[seeds::DEPOSIT, id], &LEDGER)
}
pub fn balance_pda(app: &Pubkey, session: &Pubkey) -> Pubkey {
    pda(&[seeds::BALANCE, app.as_ref(), session.as_ref()], &LEDGER)
}
pub fn pot_pda(app: &Pubkey, pot_id: &[u8; 32]) -> Pubkey {
    pda(&[seeds::POT, app.as_ref(), pot_id], &LEDGER)
}
pub fn receipt_pda(pot: &Pubkey) -> Pubkey {
    pda(&[seeds::RECEIPT, pot.as_ref()], &LEDGER)
}
pub fn settlement_pda(app: &Pubkey, seq: u64) -> Pubkey {
    pda(&[seeds::SETTLEMENT, app.as_ref(), &seq.to_le_bytes()], &LEDGER)
}

pub fn ledger_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id: LEDGER, accounts: accounts.to_account_metas(None), data: data.data() }
}

/// Wraps a ledger instruction so it reaches the ledger through `zol-test-app`,
/// signed by the app's `zol-app` address.
pub fn via_app(ix: Instruction) -> Instruction {
    let authority = app_authority(&TEST_APP);
    let mut accounts = vec![AccountMeta::new_readonly(authority, false), AccountMeta::new_readonly(LEDGER, false)];
    accounts.extend(ix.accounts.into_iter().map(|mut m| {
        if m.pubkey == authority {
            m.is_signer = false;
        }
        m
    }));
    Instruction {
        program_id: TEST_APP,
        accounts,
        data: zol_test_app::instruction::Forward { data: ix.data }.data(),
    }
}

/// Ed25519 precompile instruction: `key` signs `digest`.
pub fn ed25519_ix(key: &SigningKey, digest: &[u8; 32]) -> Instruction {
    let sig = key.sign(digest).to_bytes();
    let ix = solana_ed25519_program::new_ed25519_instruction_with_signature(digest, &sig, &key.verifying_key().to_bytes());
    Instruction { program_id: ix.program_id, accounts: vec![], data: ix.data }
}

pub fn deploy_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/deploy")
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    /// Fresh SVM with both programs and the delegation program loaded, but no config.
    pub fn bare() -> Self {
        Self::bare_from(&deploy_dir())
    }

    /// Like `bare`, with the ledger build from `dir` (e.g. target/deploy-dev).
    pub fn bare_from(dir: &std::path::Path) -> Self {
        let mut svm = LiteSVM::new();
        let ledger = std::fs::read(dir.join("zol_ledger.so")).expect("run scripts/build-sbf.sh");
        let app = std::fs::read(deploy_dir().join("zol_test_app.so")).expect("run scripts/build-sbf.sh");
        let dlp = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/dlp.so")).unwrap();
        svm.add_program(LEDGER, &ledger).unwrap();
        svm.add_program(TEST_APP, &app).unwrap();
        svm.add_program(DLP, &dlp).unwrap();
        let admin = Keypair::new();
        let relayer = Keypair::new();
        svm.airdrop(&admin.pubkey(), 100_000_000_000).unwrap();
        svm.airdrop(&relayer.pubkey(), 100_000_000_000).unwrap();
        let mut env = Env {
            svm,
            admin,
            relayer,
            attestor: SigningKey::from_bytes(&[0xA7; 32]),
            guardians: (0..3).map(|_| Keypair::new()).collect(),
            now: T0,
            network: NETWORK,
        };
        for g in &env.guardians {
            env.svm.airdrop(&g.pubkey(), 1_000_000_000).unwrap();
        }
        env.set_time(T0);
        env.set_upgrade_authority(LEDGER, env.admin.pubkey());
        env
    }

    /// Config initialized, test app registered, activated and past its delay, cap set.
    pub fn new() -> Self {
        let mut env = Self::bare();
        env.initialize().unwrap();
        env.register_test_app().unwrap();
        env.activate(&TEST_APP).unwrap();
        env.warp(params().activation_delay_secs);
        env.set_cap(&TEST_APP, u64::MAX / 4).unwrap();
        env
    }

    pub fn attestor_pubkey(&self) -> Pubkey {
        Pubkey::new_from_array(self.attestor.verifying_key().to_bytes())
    }

    pub fn set_time(&mut self, t: i64) {
        self.now = t;
        let mut c: Clock = self.svm.get_sysvar();
        c.unix_timestamp = t;
        c.slot += 1;
        self.svm.set_sysvar(&c);
        self.svm.expire_blockhash();
    }

    pub fn warp(&mut self, secs: i64) {
        self.set_time(self.now + secs);
    }

    fn set_upgrade_authority(&mut self, program: Pubkey, authority: Pubkey) {
        use solana_loader_v3_interface::state::UpgradeableLoaderState;
        let pd = Pubkey::find_program_address(&[program.as_ref()], &solana_sdk_ids::bpf_loader_upgradeable::id()).0;
        let mut acc = self.svm.get_account(&pd).unwrap();
        let meta = UpgradeableLoaderState::size_of_programdata_metadata();
        let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(authority) };
        acc.data[..meta].copy_from_slice(&bincode::serialize(&state).unwrap());
        self.svm.set_account(pd, acc).unwrap();
    }

    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> TxResult {
        let payer = signers.first().map(|k| k.pubkey()).unwrap_or(self.relayer.pubkey());
        let msg = Message::new(ixs, Some(&payer));
        let tx = Transaction::new(&signers.to_vec(), msg, self.svm.latest_blockhash());
        let r = self.svm.send_transaction(tx);
        self.svm.expire_blockhash();
        r
    }

    pub fn account<T: AccountDeserialize>(&self, key: &Pubkey) -> T {
        let a = self.svm.get_account(key).unwrap_or_else(|| panic!("missing account {key}"));
        T::try_deserialize(&mut &a.data[..]).unwrap()
    }

    pub fn try_account<T: AccountDeserialize>(&self, key: &Pubkey) -> Option<T> {
        let a = self.svm.get_account(key)?;
        T::try_deserialize(&mut &a.data[..]).ok()
    }

    pub fn raw(&self, key: &Pubkey) -> Option<Account> {
        self.svm.get_account(key)
    }

    // ------------------------------------------------------------ admin

    pub fn initialize(&mut self) -> TxResult {
        let pd = Pubkey::find_program_address(&[LEDGER.as_ref()], &solana_sdk_ids::bpf_loader_upgradeable::id()).0;
        let admin = self.admin.insecure_clone();
        self.initialize_as(&admin, pd)
    }

    pub fn initialize_as(&mut self, payer: &Keypair, program_data: Pubkey) -> TxResult {
        self.initialize_with(payer, program_data, params())
    }

    pub fn initialize_with(&mut self, payer: &Keypair, program_data: Pubkey, params: Params) -> TxResult {
        let ix = ledger_ix(
            zol_ledger::accounts::Initialize {
                payer: payer.pubkey(),
                config: config_pda(),
                program: LEDGER,
                program_data,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::Initialize {
                args: zol_ledger::InitializeArgs {
                    admin: self.admin.pubkey(),
                    attestor: self.attestor_pubkey(),
                    guardians: self.guardians.iter().map(|g| g.pubkey()).collect(),
                    guardian_threshold: 2,
                    network: self.network,
                    params,
                },
            },
        );
        self.send(&[ix], &[payer])
    }

    /// Initializes the ledger for another Zcash network (e.g. regtest).
    pub fn initialize_network(&mut self, network: u8) {
        self.network = network;
        let r = self.initialize();
        assert!(r.is_ok(), "{:?}", r.err().map(|f| f.meta.logs));
    }

    /// Activates an app with a real vault address and genesis head.
    pub fn activate_with(&mut self, app_program: &Pubkey, vault: [u8; 43], genesis_head: [u8; 32]) -> TxResult {
        let ix = ledger_ix(
            zol_ledger::accounts::ActivateApp { admin: self.admin.pubkey(), config: config_pda(), app: app_pda(app_program) },
            zol_ledger::instruction::ActivateApp { vault_address: vault, genesis_head },
        );
        let admin = self.admin.insecure_clone();
        self.send(&[ix], &[&admin])
    }

    pub fn set_config(&mut self, args: zol_ledger::SetConfigArgs) -> TxResult {
        let ix = ledger_ix(
            zol_ledger::accounts::AdminOnly { admin: self.admin.pubkey(), config: config_pda() },
            zol_ledger::instruction::SetConfig { args },
        );
        let admin = self.admin.insecure_clone();
        self.send(&[ix], &[&admin])
    }

    pub fn register_test_app(&mut self) -> TxResult {
        let ix = ledger_ix(
            zol_ledger::accounts::RegisterApp {
                payer: self.relayer.pubkey(),
                config: config_pda(),
                app_program: TEST_APP,
                app_authority: app_authority(&TEST_APP),
                app: app_pda(&TEST_APP),
                system_program: SYSTEM,
            },
            zol_ledger::instruction::RegisterApp {},
        );
        let r = self.relayer.insecure_clone();
        self.send(&[via_app(ix)], &[&r])
    }

    pub fn activate(&mut self, app_program: &Pubkey) -> TxResult {
        let ix = ledger_ix(
            zol_ledger::accounts::ActivateApp { admin: self.admin.pubkey(), config: config_pda(), app: app_pda(app_program) },
            zol_ledger::instruction::ActivateApp { vault_address: [0x33; 43], genesis_head: [0x9E; 32] },
        );
        let admin = self.admin.insecure_clone();
        self.send(&[ix], &[&admin])
    }

    pub fn set_cap(&mut self, app_program: &Pubkey, cap: u64) -> TxResult {
        let ix = ledger_ix(
            zol_ledger::accounts::SetCap { admin: self.admin.pubkey(), config: config_pda(), app: app_pda(app_program) },
            zol_ledger::instruction::SetCap { cap },
        );
        let admin = self.admin.insecure_clone();
        self.send(&[ix], &[&admin])
    }

    pub fn freeze(&mut self, guardian: usize) -> TxResult {
        let g = self.guardians[guardian].insecure_clone();
        let ix = ledger_ix(
            zol_ledger::accounts::FreezeApp { guardian: g.pubkey(), config: config_pda(), app: app_pda(&TEST_APP) },
            zol_ledger::instruction::FreezeApp {},
        );
        self.send(&[ix], &[&g])
    }

    pub fn unfreeze(&mut self, signers: &[&Keypair]) -> TxResult {
        let mut ix = ledger_ix(
            zol_ledger::accounts::UnfreezeApp { config: config_pda(), app: app_pda(&TEST_APP) },
            zol_ledger::instruction::UnfreezeApp {},
        );
        for s in signers {
            ix.accounts.push(AccountMeta::new_readonly(s.pubkey(), true));
        }
        let r = self.relayer.insecure_clone();
        let mut all: Vec<&Keypair> = vec![&r];
        all.extend_from_slice(signers);
        self.send(&[ix], &all)
    }

    // ------------------------------------------------------------ deposits

    pub fn credit_args(&self, id: [u8; 32], amount: u64, session: Pubkey, pcmt: [u8; 32], flags: u8) -> zol_ledger::CreditArgs {
        zol_ledger::CreditArgs {
            deposit_id: id,
            amount,
            session_key: session,
            payout_commitment: pcmt,
            flags,
            pot_id: [0; 32],
            app_data: vec![],
        }
    }

    pub fn credit_digest(args: &zol_ledger::CreditArgs, app_program: &Pubkey) -> [u8; 32] {
        Self::credit_digest_on(NETWORK, args, app_program)
    }

    pub fn credit_digest_on(network: u8, args: &zol_ledger::CreditArgs, app_program: &Pubkey) -> [u8; 32] {
        CreditAttestation {
            network,
            app: app_program.to_bytes(),
            deposit_id: args.deposit_id,
            amount: args.amount,
            session_key: args.session_key.to_bytes(),
            payout_commitment: args.payout_commitment,
            flags: args.flags,
            pot_id: args.pot_id,
            app_data: &args.app_data,
        }
        .digest::<Sha256>()
    }

    pub fn credit_ix(&self, args: zol_ledger::CreditArgs) -> Instruction {
        let app = app_pda(&TEST_APP);
        ledger_ix(
            zol_ledger::accounts::CreditDeposit {
                relayer: self.relayer.pubkey(),
                config: config_pda(),
                app,
                deposit: deposit_pda(&args.deposit_id),
                balance: balance_pda(&app, &args.session_key),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::CreditDeposit { args },
        )
    }

    /// Credit with a valid attestation from the configured attestor key.
    pub fn credit_with(&mut self, args: zol_ledger::CreditArgs, signer: &SigningKey) -> TxResult {
        let digest = Self::credit_digest_on(self.network, &args, &TEST_APP);
        let ix = self.credit_ix(args);
        let r = self.relayer.insecure_clone();
        self.send(&[ed25519_ix(signer, &digest), ix], &[&r])
    }

    pub fn credit(&mut self, id: [u8; 32], amount: u64, session: Pubkey, pcmt: [u8; 32]) -> TxResult {
        let args = self.credit_args(id, amount, session, pcmt, 0);
        let k = self.attestor.clone();
        self.credit_with(args, &k)
    }

    pub fn void(&mut self, id: [u8; 32]) -> TxResult {
        let digest = void_digest::<Sha256>(NETWORK, &TEST_APP.to_bytes(), &id);
        let ix = ledger_ix(
            zol_ledger::accounts::VoidDeposit {
                relayer: self.relayer.pubkey(),
                config: config_pda(),
                app: app_pda(&TEST_APP),
                deposit: deposit_pda(&id),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::VoidDeposit { deposit_id: id },
        );
        let r = self.relayer.insecure_clone();
        let k = self.attestor.clone();
        self.send(&[ed25519_ix(&k, &digest), ix], &[&r])
    }

    // ------------------------------------------------------------ pots

    pub fn open_pot(&mut self, pot_id: [u8; 32], open_secs: i64) -> TxResult {
        let app = app_pda(&TEST_APP);
        let pot = pot_pda(&app, &pot_id);
        let ix = ledger_ix(
            zol_ledger::accounts::OpenPot {
                payer: self.relayer.pubkey(),
                config: config_pda(),
                app,
                authority: app_authority(&TEST_APP),
                pot,
                receipt: receipt_pda(&pot),
                system_program: SYSTEM,
            },
            zol_ledger::instruction::OpenPot { pot_id, open_deadline: self.now + open_secs },
        );
        let r = self.relayer.insecure_clone();
        self.send(&[via_app(ix)], &[&r])
    }

    pub fn buy_in_ix(&self, pot_id: [u8; 32], session: &Pubkey, amount: u64) -> Instruction {
        let app = app_pda(&TEST_APP);
        let pot = pot_pda(&app, &pot_id);
        ledger_ix(
            zol_ledger::accounts::BuyIn {
                config: config_pda(),
                app,
                authority: app_authority(&TEST_APP),
                session_key: *session,
                balance: balance_pda(&app, session),
                pot,
                receipt: receipt_pda(&pot),
            },
            zol_ledger::instruction::BuyIn { amount },
        )
    }

    pub fn buy_in(&mut self, pot_id: [u8; 32], player: &Keypair, amount: u64) -> TxResult {
        let ix = via_app(self.buy_in_ix(pot_id, &player.pubkey(), amount));
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r, player])
    }

    pub fn leave_pot(&mut self, pot_id: [u8; 32], player: &Keypair) -> TxResult {
        let app = app_pda(&TEST_APP);
        let pot = pot_pda(&app, &pot_id);
        let ix = ledger_ix(
            zol_ledger::accounts::LeavePot {
                session_key: player.pubkey(),
                balance: balance_pda(&app, &player.pubkey()),
                pot,
                receipt: receipt_pda(&pot),
            },
            zol_ledger::instruction::LeavePot {},
        );
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r, player])
    }

    pub fn delegate_ix(&self, pot_id: [u8; 32], expected: Vec<SeatStake>, session_secs: i64) -> Instruction {
        let app = app_pda(&TEST_APP);
        let pot = pot_pda(&app, &pot_id);
        let tag = |t: &[u8]| pda(&[t, pot.as_ref()], &DLP);
        ledger_ix(
            zol_ledger::accounts::DelegatePot {
                payer: self.relayer.pubkey(),
                config: config_pda(),
                app,
                authority: app_authority(&TEST_APP),
                receipt: receipt_pda(&pot),
                pot,
                buffer_pot: pda(&[b"buffer", pot.as_ref()], &LEDGER),
                delegation_record_pot: tag(b"delegation"),
                delegation_metadata_pot: tag(b"delegation-metadata"),
                owner_program: LEDGER,
                delegation_program: DLP,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::DelegatePot { expected, session_deadline: self.now + session_secs, validator: VALIDATOR },
        )
    }

    pub fn delegate(&mut self, pot_id: [u8; 32], expected: Vec<SeatStake>, session_secs: i64) -> TxResult {
        let ix = via_app(self.delegate_ix(pot_id, expected, session_secs));
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r])
    }

    /// Stakes currently in the receipt, as `delegate_pot` expects them.
    pub fn expected(&self, pot_id: [u8; 32]) -> Vec<SeatStake> {
        let app = app_pda(&TEST_APP);
        let r: st::PotReceipt = self.account(&receipt_pda(&pot_pda(&app, &pot_id)));
        (0..r.seat_count as usize).map(|i| SeatStake { player: r.players[i], amount: r.buy_ins[i] }).collect()
    }

    /// What a MagicBlock validator does when a pot comes home: the ledger owns the
    /// account again with whatever data the validator committed.
    pub fn simulate_return(&mut self, pot_id: [u8; 32], f: impl FnOnce(&mut st::Pot)) {
        let app = app_pda(&TEST_APP);
        let key = pot_pda(&app, &pot_id);
        let mut acc = self.raw(&key).unwrap();
        // While delegated the base copy may be zeroed; rebuild from the last known state.
        let mut pot = st::Pot::try_deserialize(&mut &acc.data[..]).unwrap_or_else(|_| self.pot_snapshot(pot_id));
        f(&mut pot);
        let mut data = Vec::new();
        anchor_lang::AccountSerialize::try_serialize(&pot, &mut data).unwrap();
        data.resize(acc.data.len(), 0);
        acc.data = data;
        acc.owner = LEDGER;
        self.svm.set_account(key, acc).unwrap();
    }

    /// The pot as the receipt describes it, delegated, before any rollup move.
    pub fn pot_snapshot(&self, pot_id: [u8; 32]) -> st::Pot {
        let app = app_pda(&TEST_APP);
        let key = pot_pda(&app, &pot_id);
        let r: st::PotReceipt = self.account(&receipt_pda(&key));
        let mut amounts = [0u64; st::limits::MAX_SEATS];
        amounts.copy_from_slice(&r.buy_ins);
        st::Pot {
            app,
            app_authority: app_authority(&TEST_APP),
            pot_id,
            seat_count: r.seat_count,
            amounts,
            delegated: true,
            bump: Pubkey::find_program_address(&[seeds::POT, app.as_ref(), &pot_id], &LEDGER).1,
        }
    }

    pub fn move_seat_ix(&self, pot_id: [u8; 32], from: u8, to: u8, amount: u64) -> Instruction {
        let app = app_pda(&TEST_APP);
        let authority = app_authority(&TEST_APP);
        ledger_ix(
            zol_ledger::accounts::MoveSeat { authority, pot: pot_pda(&app, &pot_id), app_authority: authority },
            zol_ledger::instruction::MoveSeat { from, to, amount },
        )
    }

    fn outcome_ix(&self, pot_id: [u8; 32], players: &[Pubkey], data: impl InstructionData) -> Instruction {
        let app = app_pda(&TEST_APP);
        let pot = pot_pda(&app, &pot_id);
        let mut ix = ledger_ix(
            zol_ledger::accounts::PotOutcome { config: config_pda(), app, receipt: receipt_pda(&pot), pot },
            data,
        );
        for p in players {
            ix.accounts.push(AccountMeta::new(balance_pda(&app, p), false));
        }
        ix
    }

    pub fn cash_out(&mut self, pot_id: [u8; 32], players: &[Pubkey]) -> TxResult {
        let ix = self.outcome_ix(pot_id, players, zol_ledger::instruction::CashOut {});
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r])
    }

    pub fn refund_pot(&mut self, pot_id: [u8; 32], players: &[Pubkey]) -> TxResult {
        let ix = self.outcome_ix(pot_id, players, zol_ledger::instruction::RefundPot {});
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r])
    }

    // ------------------------------------------------------------ exit

    pub fn request_exit(&mut self, player: &Keypair) -> TxResult {
        let app = app_pda(&TEST_APP);
        let ix = ledger_ix(
            zol_ledger::accounts::RequestExit {
                session_key: player.pubkey(),
                config: config_pda(),
                balance: balance_pda(&app, &player.pubkey()),
            },
            zol_ledger::instruction::RequestExit {},
        );
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r, player])
    }

    pub fn settle(&mut self, players: &[Pubkey]) -> TxResult {
        let app = app_pda(&TEST_APP);
        let a: st::App = self.account(&app);
        let mut ix = ledger_ix(
            zol_ledger::accounts::Settle {
                payer: self.relayer.pubkey(),
                config: config_pda(),
                app,
                settlement: settlement_pda(&app, a.next_settlement),
                system_program: SYSTEM,
            },
            zol_ledger::instruction::Settle {},
        );
        for p in players {
            ix.accounts.push(AccountMeta::new(balance_pda(&app, p), false));
        }
        let r = self.relayer.insecure_clone();
        self.send(&[ix], &[&r])
    }

    pub fn mark_paid(&mut self, seq: u64) -> TxResult {
        let app = app_pda(&TEST_APP);
        let digest = paid_digest::<Sha256>(NETWORK, &TEST_APP.to_bytes(), seq);
        let ix = ledger_ix(
            zol_ledger::accounts::MarkPaid {
                relayer: self.relayer.pubkey(),
                config: config_pda(),
                app,
                settlement: settlement_pda(&app, seq),
                instructions: IX_SYSVAR,
            },
            zol_ledger::instruction::MarkPaid {},
        );
        let r = self.relayer.insecure_clone();
        let k = self.attestor.clone();
        self.send(&[ed25519_ix(&k, &digest), ix], &[&r])
    }

    // ------------------------------------------------------------ helpers

    /// Funds a new player with a credited deposit and returns the session keypair.
    pub fn player(&mut self, amount: u64) -> Keypair {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
        let id = rand::random::<[u8; 32]>();
        self.credit(id, amount, k.pubkey(), [7; 32]).unwrap();
        k
    }

    pub fn app_state(&self) -> st::App {
        self.account(&app_pda(&TEST_APP))
    }

    pub fn balance(&self, p: &Pubkey) -> st::Balance {
        self.account(&balance_pda(&app_pda(&TEST_APP), p))
    }

    /// Checks invariant 1 and 2 by scanning every ledger account of the test app.
    pub fn check_invariants(&self) {
        self.check_invariants_for(&TEST_APP)
    }

    /// Invariants 1 and 2 and conservation for any registered app.
    pub fn check_invariants_for(&self, app_program: &Pubkey) {
        let app_key = app_pda(app_program);
        let app: st::App = self.account(&app_key);
        let mut balances: u128 = 0;
        let mut live: u128 = 0;
        let mut credited: u128 = 0;
        let mut credited_n = 0u64;
        let mut indexes = Vec::new();
        for acc in self.svm.accounts_db().inner.values() {
            if acc.owner() != &LEDGER {
                continue;
            }
            let data = acc.data();
            if let Ok(b) = st::Balance::try_deserialize(&mut &data[..]) {
                if b.app == app_key {
                    balances += b.amount as u128;
                }
            } else if let Ok(r) = st::PotReceipt::try_deserialize(&mut &data[..]) {
                if r.app == app_key && r.is_live() {
                    live += r.total as u128;
                }
            } else if let Ok(d) = st::Deposit::try_deserialize(&mut &data[..]) {
                if d.app == app_key && d.state == st::DepositState::Credited {
                    credited += d.amount as u128;
                    credited_n += 1;
                    indexes.push(d.index);
                }
            }
        }
        assert_eq!(balances + live, app.liability as u128, "invariant 1: balances + live pots == liability");
        assert_eq!(credited, app.credited_total as u128, "invariant 2: credited total");
        assert_eq!(credited_n, app.deposit_count, "deposit count");
        indexes.sort();
        assert!(indexes.iter().enumerate().all(|(i, x)| *x == i as u64), "deposit indexes are 0..count");

        // Conservation: every credited zatoshi is owed to a player, sits in the fee
        // pool, or is in a settlement record (paid or not).
        let mut unpaid: u128 = 0;
        let mut settled: u128 = 0;
        for acc in self.svm.accounts_db().inner.values() {
            if acc.owner() != &LEDGER {
                continue;
            }
            if let Ok(s) = st::Settlement::try_deserialize(&mut &acc.data()[..]) {
                if s.app == app_key {
                    let sum: u128 = s.payouts[..s.count as usize].iter().map(|p| p.amount as u128).sum();
                    assert_eq!(sum, s.total as u128, "settlement total");
                    settled += sum;
                    if s.state == st::SettlementState::Unpaid {
                        unpaid += sum;
                    }
                }
            }
        }
        assert_eq!(unpaid, app.unpaid_total as u128, "unpaid total");
        assert_eq!(
            app.credited_total as u128,
            app.liability as u128 + app.fee_pool as u128 + settled,
            "conservation: credited == liability + fee pool + settlements"
        );
    }
}

/// Anchor custom error code of a failed transaction, if any.
pub fn err_code(r: &TxResult) -> Option<u32> {
    use solana_transaction_error::TransactionError;
    match r {
        Err(f) => match &f.err {
            TransactionError::InstructionError(_, solana_instruction::error::InstructionError::Custom(c)) => Some(*c),
            _ => None,
        },
        Ok(_) => None,
    }
}

/// Anchor error number for a `ZolError` variant.
pub fn zerr(e: zol_ledger::error::ZolError) -> u32 {
    6000 + e as u32
}

#[macro_export]
macro_rules! assert_zerr {
    ($r:expr, $e:ident) => {{
        let r = &$r;
        assert_eq!(
            $crate::err_code(r),
            Some($crate::zerr(zol_ledger::error::ZolError::$e)),
            "expected {}; got {:?}",
            stringify!($e),
            r.as_ref().map(|_| ()).map_err(|f| (&f.err, f.meta.logs.iter().rev().take(6).collect::<Vec<_>>()))
        );
    }};
}

#[macro_export]
macro_rules! ok {
    ($r:expr) => {{
        match $r {
            Ok(m) => m,
            Err(f) => panic!("tx failed: {:?}\n{}", f.err, f.meta.logs.join("\n")),
        }
    }};
}
