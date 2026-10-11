//! Real zol-ledger + zol-test-app through a full pot lifecycle on the local MagicBlock stack
//! (solana-test-validator :28899 + ephemeral-validator :27799, see ../start-local.sh).
//!
//!   cargo run --release -- setup   # initialize, register_app (via forward), activate_app
//!   cargo run --release -- run     # set_cap .. settle, after the activation delay
//!
//! Instruction and account construction mirrors tests/src/lib.rs.
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use base64::Engine;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{json, Value};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use std::time::{Duration, Instant};
use zol_ledger::state::{self as st, seeds, Params, SeatStake};
use zol_wire::digest::Sha256;
use zol_wire::CreditAttestation;

const LEDGER: Pubkey = zol_ledger::ID;
const TEST_APP: Pubkey = zol_test_app::ID;
const DLP: Pubkey = solana_pubkey::pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const MAGIC_PROGRAM: Pubkey = solana_pubkey::pubkey!("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT: Pubkey = solana_pubkey::pubkey!("MagicContext1111111111111111111111111111111");
const IX_SYSVAR: Pubkey = solana_pubkey::pubkey!("Sysvar1nstructions1111111111111111111111111");
const SYSTEM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");
const BPF_UPGRADEABLE: Pubkey = solana_pubkey::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
const NETWORK: u8 = zol_wire::Network::Testnet as u8;

const BASE: &str = "http://127.0.0.1:28899";
const ER: &str = "http://127.0.0.1:27799";

fn params() -> Params {
    Params {
        entry_fee_bps: 30,
        exit_fee_bps: 30,
        network_fee_allowance: 10_000,
        min_exit: 100_000,
        max_seats: 8,
        max_open_secs: 3_600,
        min_session_secs: 60,
        max_session_secs: 7_200,
        refund_grace_secs: if dev_timings() { 10 } else { 3_900 },
        activation_delay_secs: if dev_timings() { 5 } else { 3_600 },
        registration_bond: 0,
        denominations: [1_000_000, 10_000_000, 0, 0],
    }
}

// ------------------------------------------------------------------ rpc

fn rpc(url: &str, method: &str, params: Value) -> Value {
    for attempt in 0..5 {
        match ureq::post(url).send_json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})) {
            Ok(r) => {
                let v: Value = r.into_json().unwrap();
                if let Some(e) = v.get("error") {
                    panic!("{method} error: {e}");
                }
                return v["result"].clone();
            }
            Err(e) if attempt < 4 => {
                eprintln!("rpc {method} retry: {e}");
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => panic!("{method}: {e}"),
        }
    }
    unreachable!()
}

fn blockhash(url: &str) -> solana_hash::Hash {
    let v = rpc(url, "getLatestBlockhash", json!([{"commitment":"confirmed"}]));
    v["value"]["blockhash"].as_str().unwrap().parse().unwrap()
}

struct Sent {
    sig: String,
    ms: u128,
}

#[derive(Debug)]
struct Failed {
    sig: String,
    err: Value,
    logs: Vec<String>,
}

fn tx_logs(url: &str, sig: &str) -> Vec<String> {
    for _ in 0..20 {
        let v = rpc(url, "getTransaction", json!([sig, {"encoding":"json","commitment":"confirmed","maxSupportedTransactionVersion":0}]));
        if !v.is_null() {
            return v["meta"]["logMessages"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect()).unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    vec![]
}

fn send(url: &str, ixs: &[Instruction], signers: &[&Keypair]) -> Result<Sent, Failed> {
    let payer = signers[0].pubkey();
    let msg = Message::new(ixs, Some(&payer));
    let tx = Transaction::new(&signers.to_vec(), msg, blockhash(url));
    let raw = base64::engine::general_purpose::STANDARD.encode(bincode::serialize(&tx).unwrap());
    let t0 = Instant::now();
    let res = ureq::post(url)
        .send_json(json!({"jsonrpc":"2.0","id":1,"method":"sendTransaction","params":[raw, {"encoding":"base64","skipPreflight":true}]}))
        .unwrap()
        .into_json::<Value>()
        .unwrap();
    if let Some(e) = res.get("error") {
        return Err(Failed { sig: String::new(), err: e.clone(), logs: vec![] });
    }
    let sig = res["result"].as_str().unwrap().to_string();
    loop {
        let st = rpc(url, "getSignatureStatuses", json!([[sig], {"searchTransactionHistory": false}]));
        let s = &st["value"][0];
        if !s.is_null() {
            if !s["err"].is_null() {
                let logs = tx_logs(url, &sig);
                return Err(Failed { sig, err: s["err"].clone(), logs });
            }
            let c = s["confirmationStatus"].as_str().unwrap_or("");
            if c == "confirmed" || c == "finalized" {
                return Ok(Sent { sig, ms: t0.elapsed().as_millis() });
            }
        }
        if t0.elapsed() > Duration::from_secs(60) {
            return Err(Failed { sig, err: json!("timeout"), logs: vec![] });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn account(url: &str, key: &Pubkey) -> Option<(Pubkey, Vec<u8>, u64)> {
    let v = rpc(url, "getAccountInfo", json!([key.to_string(), {"encoding":"base64","commitment":"confirmed"}]));
    let a = &v["value"];
    if a.is_null() {
        return None;
    }
    let data = base64::engine::general_purpose::STANDARD.decode(a["data"][0].as_str().unwrap()).unwrap();
    Some((a["owner"].as_str().unwrap().parse().unwrap(), data, a["lamports"].as_u64().unwrap()))
}

fn load<T: AccountDeserialize>(url: &str, key: &Pubkey) -> T {
    let (_, data, _) = account(url, key).unwrap_or_else(|| panic!("missing {key}"));
    T::try_deserialize(&mut &data[..]).unwrap()
}

fn now_on_chain(url: &str) -> i64 {
    let (_, d, _) = account(url, &solana_pubkey::pubkey!("SysvarC1ock11111111111111111111111111111111")).unwrap();
    i64::from_le_bytes(d[32..40].try_into().unwrap())
}

fn airdrop(key: &Pubkey, lamports: u64) {
    let sig = rpc(BASE, "requestAirdrop", json!([key.to_string(), lamports]));
    let sig = sig.as_str().unwrap().to_string();
    for _ in 0..100 {
        let st = rpc(BASE, "getSignatureStatuses", json!([[sig]]));
        if st["value"][0]["confirmationStatus"].as_str() == Some("confirmed") || st["value"][0]["confirmationStatus"].as_str() == Some("finalized") {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("airdrop not confirmed");
}

// ------------------------------------------------------------------ keys and pdas

fn spike_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn read_kp(path: &std::path::Path) -> Keypair {
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    Keypair::try_from(&bytes[..]).unwrap()
}
fn kp(name: &str) -> Keypair {
    read_kp(&spike_dir().join("keys").join(format!("{name}.json")))
}
fn kp_or_new(name: &str) -> Keypair {
    let p = spike_dir().join("keys").join(format!("{name}.json"));
    if p.exists() {
        return read_kp(&p);
    }
    let k = Keypair::new();
    std::fs::write(&p, serde_json::to_string(&k.to_bytes().to_vec()).unwrap()).unwrap();
    k
}
fn attestor() -> SigningKey {
    SigningKey::from_bytes(&[0xA7; 32]) // same throwaway attestor as tests/src/lib.rs
}
fn attestor_pubkey() -> Pubkey {
    Pubkey::new_from_array(attestor().verifying_key().to_bytes())
}

fn pda(s: &[&[u8]], p: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(s, p).0
}
fn config_pda() -> Pubkey {
    pda(&[seeds::CONFIG], &LEDGER)
}
fn app_authority() -> Pubkey {
    pda(&[seeds::APP_AUTHORITY], &TEST_APP)
}
fn app_pda() -> Pubkey {
    pda(&[seeds::APP, TEST_APP.as_ref()], &LEDGER)
}
fn deposit_pda(id: &[u8; 32]) -> Pubkey {
    pda(&[seeds::DEPOSIT, id], &LEDGER)
}
fn balance_pda(session: &Pubkey) -> Pubkey {
    pda(&[seeds::BALANCE, app_pda().as_ref(), session.as_ref()], &LEDGER)
}
fn pot_pda(pot_id: &[u8; 32]) -> Pubkey {
    pda(&[seeds::POT, app_pda().as_ref(), pot_id], &LEDGER)
}
fn receipt_pda(pot: &Pubkey) -> Pubkey {
    pda(&[seeds::RECEIPT, pot.as_ref()], &LEDGER)
}
fn settlement_pda(seq: u64) -> Pubkey {
    pda(&[seeds::SETTLEMENT, app_pda().as_ref(), &seq.to_le_bytes()], &LEDGER)
}

fn ledger_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id: LEDGER, accounts: accounts.to_account_metas(None), data: data.data() }
}

/// Same as tests/src/lib.rs `via_app`: route through zol-test-app `forward`.
fn via_app(ix: Instruction) -> Instruction {
    let authority = app_authority();
    let mut accounts = vec![AccountMeta::new_readonly(authority, false), AccountMeta::new_readonly(LEDGER, false)];
    accounts.extend(ix.accounts.into_iter().map(|mut m| {
        if m.pubkey == authority {
            m.is_signer = false;
        }
        m
    }));
    Instruction { program_id: TEST_APP, accounts, data: zol_test_app::instruction::Forward { data: ix.data }.data() }
}

fn ed25519_ix(key: &SigningKey, digest: &[u8; 32]) -> Instruction {
    let sig = key.sign(digest).to_bytes();
    let ix = solana_ed25519_program::new_ed25519_instruction_with_signature(digest, &sig, &key.verifying_key().to_bytes());
    Instruction { program_id: ix.program_id, accounts: vec![], data: ix.data }
}

// ------------------------------------------------------------------ steps

macro_rules! step {
    ($name:expr, $r:expr) => {{
        match $r {
            Ok(s) => {
                println!("{}", json!({"step": $name, "ok": true, "sig": s.sig, "ms": s.ms as u64}));
                s
            }
            Err(f) => {
                println!("{}", json!({"step": $name, "ok": false, "sig": f.sig, "err": f.err, "logs": f.logs}));
                std::process::exit(1);
            }
        }
    }};
}

fn setup() {
    let admin = kp("payer"); // upgrade authority of the ledger in start-local.sh
    let guardians: Vec<Keypair> = (0..3).map(|i| kp_or_new(&format!("e2e-guardian{i}"))).collect();
    let pd = pda(&[LEDGER.as_ref()], &BPF_UPGRADEABLE);
    let ix = ledger_ix(
        zol_ledger::accounts::Initialize { payer: admin.pubkey(), config: config_pda(), program: LEDGER, program_data: pd, system_program: SYSTEM },
        zol_ledger::instruction::Initialize {
            args: zol_ledger::InitializeArgs {
                admin: admin.pubkey(),
                attestor: attestor_pubkey(),
                guardians: guardians.iter().map(|g| g.pubkey()).collect(),
                guardian_threshold: 2,
                network: NETWORK,
                params: params(),
            },
        },
    );
    step!("initialize", send(BASE, &[ix], &[&admin]));

    let ix = ledger_ix(
        zol_ledger::accounts::RegisterApp {
            payer: admin.pubkey(),
            config: config_pda(),
            app_program: TEST_APP,
            app_authority: app_authority(),
            app: app_pda(),
            system_program: SYSTEM,
        },
        zol_ledger::instruction::RegisterApp {},
    );
    step!("register_app (forward)", send(BASE, &[via_app(ix)], &[&admin]));

    let ix = ledger_ix(
        zol_ledger::accounts::ActivateApp { admin: admin.pubkey(), config: config_pda(), app: app_pda() },
        zol_ledger::instruction::ActivateApp { vault_address: [0x33; 43], genesis_head: [0x9E; 32] },
    );
    step!("activate_app", send(BASE, &[ix], &[&admin]));
    let a: st::App = load(BASE, &app_pda());
    println!("{}", json!({"app_state": format!("{:?}", a.state), "chain_now": now_on_chain(BASE), "activation_delay_secs": params().activation_delay_secs}));
}

fn run() {
    let admin = kp("payer");
    let relayer = kp("payer");
    let nosol = kp("nosol");
    let er_identity: Pubkey = rpc(ER, "getIdentity", json!([]))["identity"].as_str().unwrap().parse().unwrap();
    println!("{}", json!({"chain_now": now_on_chain(BASE), "er_identity": er_identity.to_string(), "nosol_base_lamports": account(BASE, &nosol.pubkey()).map(|a| a.2).unwrap_or(0)}));

    // set_cap
    let ix = ledger_ix(
        zol_ledger::accounts::SetCap { admin: admin.pubkey(), config: config_pda(), app: app_pda() },
        zol_ledger::instruction::SetCap { cap: u64::MAX / 4 },
    );
    step!("set_cap", send(BASE, &[ix], &[&admin]));

    // two players, two attested credits
    let players: Vec<Keypair> = (0..2).map(|_| Keypair::new()).collect();
    let deposit = 1_000_000u64;
    for (i, p) in players.iter().enumerate() {
        let args = zol_ledger::CreditArgs {
            deposit_id: rand::random(),
            amount: deposit,
            session_key: p.pubkey(),
            payout_commitment: [7 + i as u8; 32],
            flags: 0,
            pot_id: [0; 32],
            app_data: vec![],
        };
        let digest = CreditAttestation {
            network: NETWORK,
            app: TEST_APP.to_bytes(),
            deposit_id: args.deposit_id,
            amount: args.amount,
            session_key: args.session_key.to_bytes(),
            payout_commitment: args.payout_commitment,
            flags: args.flags,
            pot_id: args.pot_id,
            app_data: &args.app_data,
        }
        .digest::<Sha256>();
        let ix = ledger_ix(
            zol_ledger::accounts::CreditDeposit {
                relayer: relayer.pubkey(),
                config: config_pda(),
                app: app_pda(),
                deposit: deposit_pda(&args.deposit_id),
                balance: balance_pda(&p.pubkey()),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::CreditDeposit { args },
        );
        step!(format!("credit_deposit p{i}"), send(BASE, &[ed25519_ix(&attestor(), &digest), ix], &[&relayer]));
    }

    // open_pot
    let pot_id: [u8; 32] = rand::random();
    let pot = pot_pda(&pot_id);
    let now = now_on_chain(BASE);
    let ix = ledger_ix(
        zol_ledger::accounts::OpenPot {
            payer: relayer.pubkey(),
            config: config_pda(),
            app: app_pda(),
            authority: app_authority(),
            pot,
            receipt: receipt_pda(&pot),
            system_program: SYSTEM,
        },
        zol_ledger::instruction::OpenPot { pot_id, open_deadline: now + 1_800 },
    );
    step!("open_pot (forward)", send(BASE, &[via_app(ix)], &[&relayer]));

    // buy_in x2
    let stake = 500_000u64;
    for (i, p) in players.iter().enumerate() {
        let ix = ledger_ix(
            zol_ledger::accounts::BuyIn {
                config: config_pda(),
                app: app_pda(),
                authority: app_authority(),
                session_key: p.pubkey(),
                balance: balance_pda(&p.pubkey()),
                pot,
                receipt: receipt_pda(&pot),
            },
            zol_ledger::instruction::BuyIn { amount: stake },
        );
        step!(format!("buy_in p{i} (forward + session key)"), send(BASE, &[via_app(ix)], &[&relayer, p]));
    }

    // delegate_pot, pinned to the local ER identity
    let r: st::PotReceipt = load(BASE, &receipt_pda(&pot));
    let expected: Vec<SeatStake> = (0..r.seat_count as usize).map(|i| SeatStake { player: r.players[i], amount: r.buy_ins[i] }).collect();
    let tag = |t: &[u8]| pda(&[t, pot.as_ref()], &DLP);
    let ix = ledger_ix(
        zol_ledger::accounts::DelegatePot {
            payer: relayer.pubkey(),
            config: config_pda(),
            app: app_pda(),
            authority: app_authority(),
            receipt: receipt_pda(&pot),
            pot,
            buffer_pot: pda(&[b"buffer", pot.as_ref()], &LEDGER),
            delegation_record_pot: tag(b"delegation"),
            delegation_metadata_pot: tag(b"delegation-metadata"),
            owner_program: LEDGER,
            delegation_program: DLP,
            system_program: SYSTEM,
        },
        zol_ledger::instruction::DelegatePot { expected, session_deadline: now_on_chain(BASE) + 600, validator: er_identity },
    );
    step!("delegate_pot (forward)", send(BASE, &[via_app(ix)], &[&relayer]));
    let t0 = Instant::now();
    let owner_base = account(BASE, &pot).unwrap().0;
    loop {
        if let Some((o, _, _)) = account(ER, &pot) {
            if o == LEDGER {
                break;
            }
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "pot never visible in ER");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("{}", json!({"pot": pot.to_string(), "base_owner_while_delegated": owner_base.to_string(), "er_visible_ms": t0.elapsed().as_millis() as u64}));

    // move_seat in the ER (seat 0 -> seat 1), via forward; fee payer = zero-SOL key
    let mv = 200_000u64;
    let authority = app_authority();
    let ix = ledger_ix(
        zol_ledger::accounts::MoveSeat { authority, pot, app_authority: authority },
        zol_ledger::instruction::MoveSeat { from: 0, to: 1, amount: mv },
    );
    step!("move_seat in ER (forward, fee payer nosol)", send(ER, &[via_app(ix)], &[&nosol]));
    let p_er: st::Pot = load(ER, &pot);
    println!("{}", json!({"er_pot_amounts": &p_er.amounts[..p_er.seat_count as usize], "er_pot_delegated": p_er.delegated}));

    // close_pot in the ER via forward; payer = zero-SOL key
    let ix = ledger_ix(
        zol_ledger::accounts::ClosePot { payer: nosol.pubkey(), authority, pot, magic_fee_vault: None, escrow_authority: None, magic_program: MAGIC_PROGRAM, magic_context: MAGIC_CONTEXT },
        zol_ledger::instruction::ClosePot { action: None },
    );
    let tclose = Instant::now();
    let close = step!("close_pot in ER (forward, payer nosol)", send(ER, &[via_app(ix)], &[&nosol]));
    let er_logs = tx_logs(ER, &close.sig);
    println!("{}", json!({"close_pot_er_logs": er_logs}));

    // wait for return
    loop {
        if let Some((o, _, _)) = account(BASE, &pot) {
            if o == LEDGER {
                break;
            }
        }
        if tclose.elapsed() > Duration::from_secs(120) {
            println!("{}", json!({"step": "return", "ok": false, "base_owner": account(BASE, &pot).map(|a| a.0.to_string())}));
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let returned_ms = tclose.elapsed().as_millis() as u64;
    let p_base: st::Pot = load(BASE, &pot);
    println!("{}", json!({"step": "return", "ok": true, "close_to_owner_ledger_ms": returned_ms, "base_pot_amounts": &p_base.amounts[..p_base.seat_count as usize], "base_pot_delegated": p_base.delegated}));

    // cash_out
    let mut ix = ledger_ix(
        zol_ledger::accounts::PotOutcome { config: config_pda(), app: app_pda(), receipt: receipt_pda(&pot), pot },
        zol_ledger::instruction::CashOut {},
    );
    for p in &players {
        ix.accounts.push(AccountMeta::new(balance_pda(&p.pubkey()), false));
    }
    step!("cash_out", send(BASE, &[ix], &[&relayer]));
    println!("{}", json!({"close_to_cash_out_confirmed_ms": tclose.elapsed().as_millis() as u64}));
    let bal: Vec<u64> = players.iter().map(|p| load::<st::Balance>(BASE, &balance_pda(&p.pubkey())).amount).collect();
    let fee_in = st::bps(deposit, params().entry_fee_bps);
    let want = [deposit - fee_in - stake + (stake - mv), deposit - fee_in - stake + (stake + mv)];
    println!("{}", json!({"balances_after_cash_out": bal, "expected": want, "match": bal == want}));

    // request_exit x2, settle
    for (i, p) in players.iter().enumerate() {
        let ix = ledger_ix(
            zol_ledger::accounts::RequestExit { session_key: p.pubkey(), config: config_pda(), balance: balance_pda(&p.pubkey()) },
            zol_ledger::instruction::RequestExit {},
        );
        step!(format!("request_exit p{i}"), send(BASE, &[ix], &[&relayer, p]));
    }
    let a: st::App = load(BASE, &app_pda());
    let seq = a.next_settlement;
    let mut ix = ledger_ix(
        zol_ledger::accounts::Settle { payer: relayer.pubkey(), config: config_pda(), app: app_pda(), settlement: settlement_pda(seq), system_program: SYSTEM },
        zol_ledger::instruction::Settle {},
    );
    for p in &players {
        ix.accounts.push(AccountMeta::new(balance_pda(&p.pubkey()), false));
    }
    step!("settle", send(BASE, &[ix], &[&relayer]));
    let s: st::Settlement = load(BASE, &settlement_pda(seq));
    let a: st::App = load(BASE, &app_pda());
    let pay = |b: u64| b - st::bps(b, params().exit_fee_bps) - params().network_fee_allowance;
    let want_pay = [pay(want[0]), pay(want[1])];
    let got: Vec<u64> = s.payouts[..s.count as usize].iter().map(|p| p.amount).collect();
    let bal_after: Vec<u64> = players.iter().map(|p| load::<st::Balance>(BASE, &balance_pda(&p.pubkey())).amount).collect();
    println!(
        "{}",
        json!({
            "settlement_seq": seq, "settlement_state": format!("{:?}", s.state), "payouts": got, "expected_payouts": want_pay,
            "payouts_match": got == want_pay, "settlement_total": s.total, "balances_after_settle": bal_after,
            "app": {"liability": a.liability, "credited_total": a.credited_total, "fee_pool": a.fee_pool, "unpaid_total": a.unpaid_total, "deposit_count": a.deposit_count},
            "conservation_credited_eq_liability_fee_unpaid": a.credited_total == a.liability + a.fee_pool + a.unpaid_total, // no mark_paid in this harness
        })
    );
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("setup") => setup(),
        Some("run") => run(),
        Some("airdrop") => {
            for n in ["payer"] {
                airdrop(&kp(n).pubkey(), 100_000_000_000);
            }
        }
        _ => eprintln!("usage: ledger-e2e setup|run|airdrop"),
    }
}

/// `ZOL_DEV_TIMINGS=1` with a ledger from target/deploy-dev (`dev-timings` build):
/// 5 s activation delay and 10 s refund grace instead of an hour.
fn dev_timings() -> bool {
    std::env::var("ZOL_DEV_TIMINGS").is_ok_and(|v| v == "1")
}
