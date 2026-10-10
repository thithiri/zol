//! zol-coinflip end to end on the local MagicBlock stack (base :28899, ER :27799, VRF oracle).
//!
//!   cargo run --release --bin coinflip -- setup   # initialize ledger, register coinflip, activate
//!   cargo run --release --bin coinflip -- play    # after the 3600 s activation delay
//!
//! The coinflip program must be deployed first (upgradeable, any authority):
//!   solana program deploy ../../target/deploy/zol_coinflip.so --program-id ../../keys/zol_coinflip-keypair.json
#[path = "../common.rs"]
mod common;

use anchor_lang::{InstructionData, ToAccountMetas};
use common::*;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::json;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use std::time::{Duration, Instant};
use zol_coinflip::{Table, TableState};
use zol_ledger::state::{self as st, seeds, Params};
use zol_wire::digest::Sha256;
use zol_wire::CreditAttestation;

const LEDGER: Pubkey = zol_ledger::ID;
const APPP: Pubkey = zol_coinflip::ID;
const DLP: Pubkey = solana_pubkey::pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const MAGIC_PROGRAM: Pubkey = solana_pubkey::pubkey!("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT: Pubkey = solana_pubkey::pubkey!("MagicContext1111111111111111111111111111111");
const IX_SYSVAR: Pubkey = solana_pubkey::pubkey!("Sysvar1nstructions1111111111111111111111111");
const SYSTEM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");
const BPF_UPGRADEABLE: Pubkey = solana_pubkey::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
const VRF_PROGRAM: Pubkey = solana_pubkey::pubkey!("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz");
const SLOT_HASHES: Pubkey = solana_pubkey::pubkey!("SysvarS1otHashes111111111111111111111111111");
const COMPUTE_BUDGET: Pubkey = solana_pubkey::pubkey!("ComputeBudget111111111111111111111111111111");
const NETWORK: u8 = zol_wire::Network::Testnet as u8;

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

fn spike_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn kp(name: &str) -> Keypair {
    let p = spike_dir().join("keys").join(format!("{name}.json"));
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    Keypair::try_from(&bytes[..]).unwrap()
}
fn kp_or_new(name: &str) -> Keypair {
    let p = spike_dir().join("keys").join(format!("{name}.json"));
    if p.exists() {
        return kp(name);
    }
    let k = Keypair::new();
    std::fs::write(&p, serde_json::to_string(&k.to_bytes().to_vec()).unwrap()).unwrap();
    k
}
fn attestor() -> SigningKey {
    SigningKey::from_bytes(&[0xA7; 32])
}
fn pda(s: &[&[u8]], p: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(s, p).0
}
fn config_pda() -> Pubkey {
    pda(&[seeds::CONFIG], &LEDGER)
}
fn app_authority() -> Pubkey {
    pda(&[seeds::APP_AUTHORITY], &APPP)
}
fn app_pda() -> Pubkey {
    pda(&[seeds::APP, APPP.as_ref()], &LEDGER)
}
fn balance_pda(s: &Pubkey) -> Pubkey {
    pda(&[seeds::BALANCE, app_pda().as_ref(), s.as_ref()], &LEDGER)
}
fn pot_pda(id: &[u8; 32]) -> Pubkey {
    pda(&[seeds::POT, app_pda().as_ref(), id], &LEDGER)
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
fn app_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id: APPP, accounts: accounts.to_account_metas(None), data: data.data() }
}
fn cu(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET, accounts: vec![], data }
}
fn ed25519_ix(key: &SigningKey, digest: &[u8; 32]) -> Instruction {
    let sig = key.sign(digest).to_bytes();
    let ix = solana_ed25519_program::new_ed25519_instruction_with_signature(digest, &sig, &key.verifying_key().to_bytes());
    Instruction { program_id: ix.program_id, accounts: vec![], data: ix.data }
}

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

fn wait<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> (T, u64) {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return (v, t0.elapsed().as_millis() as u64);
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            println!("{}", json!({"step": what, "ok": false, "err": "timeout"}));
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn setup() {
    let admin = kp("payer");
    airdrop(&admin.pubkey(), 100_000_000_000);
    let guardians: Vec<Keypair> = (0..3).map(|i| kp_or_new(&format!("e2e-guardian{i}"))).collect();
    let pd = pda(&[LEDGER.as_ref()], &BPF_UPGRADEABLE);
    let ix = ledger_ix(
        zol_ledger::accounts::Initialize { payer: admin.pubkey(), config: config_pda(), program: LEDGER, program_data: pd, system_program: SYSTEM },
        zol_ledger::instruction::Initialize {
            args: zol_ledger::InitializeArgs {
                admin: admin.pubkey(),
                attestor: Pubkey::new_from_array(attestor().verifying_key().to_bytes()),
                guardians: guardians.iter().map(|g| g.pubkey()).collect(),
                guardian_threshold: 2,
                network: NETWORK,
                params: params(),
            },
        },
    );
    step!("initialize", send(BASE, &[ix], &[&admin]));
    let ix = app_ix(
        zol_coinflip::accounts::Register {
            payer: admin.pubkey(),
            this_program: APPP,
            app_authority: app_authority(),
            config: config_pda(),
            app: app_pda(),
            ledger_program: LEDGER,
            system_program: SYSTEM,
        },
        zol_coinflip::instruction::Register {},
    );
    step!("coinflip.register (CPI register_app)", send(BASE, &[ix], &[&admin]));
    let ix = ledger_ix(
        zol_ledger::accounts::ActivateApp { admin: admin.pubkey(), config: config_pda(), app: app_pda() },
        zol_ledger::instruction::ActivateApp { vault_address: [0x33; 43], genesis_head: [0x9E; 32] },
    );
    step!("activate_app", send(BASE, &[ix], &[&admin]));
    println!("{}", json!({"chain_now": now_on_chain(BASE), "active_after": now_on_chain(BASE) + params().activation_delay_secs}));
}

fn play() {
    let admin = kp("payer");
    let relayer = kp("payer");
    let nosol = kp("nosol");
    let queue: Pubkey = std::env::var("VRF_QUEUE").unwrap_or("Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT".into()).parse().unwrap();
    let er_identity: Pubkey = rpc(ER, "getIdentity", json!([]))["identity"].as_str().unwrap().parse().unwrap();
    println!("{}", json!({"er_identity": er_identity.to_string(), "vrf_queue": queue.to_string(), "chain_now": now_on_chain(BASE)}));

    let ix = ledger_ix(
        zol_ledger::accounts::SetCap { admin: admin.pubkey(), config: config_pda(), app: app_pda() },
        zol_ledger::instruction::SetCap { cap: u64::MAX / 4 },
    );
    step!("set_cap", send(BASE, &[ix], &[&admin]));

    let deposit = 1_000_000u64;
    let players: Vec<Keypair> = (0..2).map(|_| Keypair::new()).collect();
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
            app: APPP.to_bytes(),
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
                deposit: pda(&[seeds::DEPOSIT, &args.deposit_id], &LEDGER),
                balance: balance_pda(&p.pubkey()),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::CreditDeposit { args },
        );
        step!(format!("credit_deposit p{i}"), send(BASE, &[ed25519_ix(&attestor(), &digest), ix], &[&relayer]));
    }

    // create_table
    let pot_id: [u8; 32] = rand::random();
    let pot = pot_pda(&pot_id);
    let table = zol_coinflip::table_address(&pot_id);
    // One deposit's net credited value: the only stakes create_table accepts.
    let stake = zol_coinflip::allowed_stakes(params().entry_fee_bps)[0];
    let ix = app_ix(
        zol_coinflip::accounts::CreateTable {
            payer: relayer.pubkey(),
            table,
            app_authority: app_authority(),
            config: config_pda(),
            app: app_pda(),
            pot,
            receipt: receipt_pda(&pot),
            ledger_program: LEDGER,
            system_program: SYSTEM,
        },
        zol_coinflip::instruction::CreateTable { pot_id, stake, open_secs: 1_800, validator: er_identity },
    );
    step!("create_table (CPI open_pot)", send(BASE, &[ix], &[&relayer]));

    // join x2: the session key signs, the relayer pays the Solana fee
    for (i, p) in players.iter().enumerate() {
        let ix = app_ix(
            zol_coinflip::accounts::Join {
                session_key: p.pubkey(),
                table,
                app_authority: app_authority(),
                config: config_pda(),
                app: app_pda(),
                balance: balance_pda(&p.pubkey()),
                pot,
                receipt: receipt_pda(&pot),
                ledger_program: LEDGER,
            },
            zol_coinflip::instruction::Join {},
        );
        step!(format!("join p{i} (CPI buy_in)"), send(BASE, &[ix], &[&relayer, p]));
    }

    // start: delegate pot (through the ledger) and table to the local ER
    let tag = |t: &[u8], k: &Pubkey| pda(&[t, k.as_ref()], &DLP);
    let ix = app_ix(
        zol_coinflip::accounts::Start {
            payer: relayer.pubkey(),
            buffer_table: pda(&[b"buffer", table.as_ref()], &APPP),
            delegation_record_table: tag(b"delegation", &table),
            delegation_metadata_table: tag(b"delegation-metadata", &table),
            table,
            app_authority: app_authority(),
            config: config_pda(),
            app: app_pda(),
            receipt: receipt_pda(&pot),
            pot,
            ledger_buffer_pot: pda(&[b"buffer", pot.as_ref()], &LEDGER),
            ledger_delegation_record_pot: tag(b"delegation", &pot),
            ledger_delegation_metadata_pot: tag(b"delegation-metadata", &pot),
            ledger_program: LEDGER,
            owner_program: APPP,
            delegation_program: DLP,
            system_program: SYSTEM,
        },
        zol_coinflip::instruction::Start { session_secs: 600 },
    );
    step!("start (CPI delegate_pot + delegate table)", send(BASE, &[cu(400_000), ix], &[&relayer]));
    let (_, ms) = wait("pot+table visible in ER", 30, || {
        let p = account(ER, &pot).filter(|a| a.0 == LEDGER)?;
        let t = account(ER, &table).filter(|a| a.0 == APPP)?;
        Some((p, t))
    });
    println!("{}", json!({"pot": pot.to_string(), "table": table.to_string(), "er_visible_ms": ms,
        "base_owner_pot": account(BASE, &pot).unwrap().0.to_string(), "base_owner_table": account(BASE, &table).unwrap().0.to_string()}));

    // flip in the ER; fee payer and VRF payer = zero-SOL key
    let flip_payer = if std::env::var("FLIP_PAYER_FUNDED").is_ok() { &relayer } else { &nosol };
    let ix = app_ix(
        zol_coinflip::accounts::Flip {
            payer: flip_payer.pubkey(),
            table,
            oracle_queue: queue,
            program_identity: pda(&[b"identity"], &APPP),
            vrf_program: VRF_PROGRAM,
            slot_hashes: SLOT_HASHES,
            system_program: SYSTEM,
        },
        zol_coinflip::instruction::Flip { caller_seed: rand::random() },
    );
    let tflip = Instant::now();
    step!("flip in ER (VRF request)", send(ER, &[ix], &[flip_payer]));
    let (t, _) = wait("land (VRF callback) in ER", 90, || {
        let t: Table = account(ER, &table).and_then(|a| anchor_lang::AccountDeserialize::try_deserialize(&mut &a.1[..]).ok())?;
        (t.state == TableState::Landed).then_some(t)
    });
    let land_ms = tflip.elapsed().as_millis() as u64;
    let p_er: st::Pot = load(ER, &pot);
    println!("{}", json!({"step": "land", "ok": true, "flip_to_land_ms": land_ms, "winner_seat": t.winner, "er_pot_amounts": &p_er.amounts[..2]}));

    // finish in the ER; payer = zero-SOL key
    let ix = app_ix(
        zol_coinflip::accounts::Finish {
            payer: nosol.pubkey(),
            table,
            pot,
            app_authority: app_authority(),
            ledger_program: LEDGER,
            magic_program: MAGIC_PROGRAM,
            magic_context: MAGIC_CONTEXT,
        },
        zol_coinflip::instruction::Finish {},
    );
    let tfin = Instant::now();
    let f = step!("finish in ER (CPI close_pot + undelegate table)", send(ER, &[ix], &[&nosol]));
    println!("{}", json!({"finish_er_logs": tx_logs(ER, &f.sig)}));
    let (_, ret_ms) = wait("pot and table back on base", 120, || {
        let p = account(BASE, &pot).filter(|a| a.0 == LEDGER)?;
        let t = account(BASE, &table).filter(|a| a.0 == APPP)?;
        Some((p, t))
    });
    let tb: Table = load(BASE, &table);
    println!("{}", json!({"step": "return", "ok": true, "finish_to_both_owned_ms": ret_ms, "base_table_state": format!("{:?}", tb.state), "base_table_winner": tb.winner}));

    // cash_out: anyone; balances in receipt order
    let r: st::PotReceipt = load(BASE, &receipt_pda(&pot));
    let mut ix = ledger_ix(
        zol_ledger::accounts::PotOutcome { config: config_pda(), app: app_pda(), receipt: receipt_pda(&pot), pot },
        zol_ledger::instruction::CashOut {},
    );
    for i in 0..r.seat_count as usize {
        ix.accounts.push(AccountMeta::new(balance_pda(&r.players[i]), false));
    }
    step!("cash_out", send(BASE, &[ix], &[&relayer]));
    println!("{}", json!({"finish_to_cash_out_ms": tfin.elapsed().as_millis() as u64}));
    let net = deposit - zol_ledger::state::bps(deposit, params().entry_fee_bps);
    let winner_key = r.players[tb.winner as usize];
    let bal: Vec<(String, u64)> = players.iter().map(|p| (p.pubkey().to_string(), load::<st::Balance>(BASE, &balance_pda(&p.pubkey())).amount)).collect();
    let want = |k: &Pubkey| if *k == winner_key { net - stake + 2 * stake } else { net - stake };
    let ok = players.iter().zip(&bal).all(|(p, b)| b.1 == want(&p.pubkey()));
    println!("{}", json!({"winner": winner_key.to_string(), "balances_after_cash_out": bal, "match": ok}));

    // close_table: anyone (here the zero-SOL key cannot pay a Solana fee, so a fresh
    // funded stranger does); the rent goes to the creator (the relayer).
    let stranger = Keypair::new();
    airdrop(&stranger.pubkey(), 1_000_000_000);
    let rent = account(BASE, &table).map(|a| a.2).unwrap_or(0);
    let before = account(BASE, &relayer.pubkey()).unwrap().2;
    let ix = app_ix(
        zol_coinflip::accounts::CloseTable { table, receipt: receipt_pda(&pot), creator: relayer.pubkey() },
        zol_coinflip::instruction::CloseTable {},
    );
    step!("close_table (anyone; rent to the creator)", send(BASE, &[ix], &[&stranger]));
    let after = account(BASE, &relayer.pubkey()).unwrap().2;
    println!("{}", json!({"table_rent": rent, "creator_gain": after - before, "table_gone": account(BASE, &table).is_none()}));

    // The loser has nothing left; only the winner can exit (minimum exit).
    let winner = players.iter().find(|p| p.pubkey() == winner_key).unwrap();
    let loser = players.iter().find(|p| p.pubkey() != winner_key).unwrap();
    let exit_ix = |p: &Keypair| ledger_ix(
        zol_ledger::accounts::RequestExit { session_key: p.pubkey(), config: config_pda(), balance: balance_pda(&p.pubkey()) },
        zol_ledger::instruction::RequestExit {},
    );
    let refused = send(BASE, &[exit_ix(loser)], &[&relayer, loser]).is_err();
    println!("{}", json!({"step": "request_exit loser (balance 0)", "refused": refused}));
    step!("request_exit winner", send(BASE, &[exit_ix(winner)], &[&relayer, winner]));
    let players = vec![winner.insecure_clone()];
    let a: st::App = load(BASE, &app_pda());
    let mut ix = ledger_ix(
        zol_ledger::accounts::Settle { payer: relayer.pubkey(), config: config_pda(), app: app_pda(), settlement: settlement_pda(a.next_settlement), system_program: SYSTEM },
        zol_ledger::instruction::Settle {},
    );
    for p in &players {
        ix.accounts.push(AccountMeta::new(balance_pda(&p.pubkey()), false));
    }
    step!("settle", send(BASE, &[ix], &[&relayer]));
    let s: st::Settlement = load(BASE, &settlement_pda(a.next_settlement));
    let a: st::App = load(BASE, &app_pda());
    println!("{}", json!({"settlement_payouts": s.payouts[..s.count as usize].iter().map(|p| p.amount).collect::<Vec<_>>(),
        "app": {"credited_total": a.credited_total, "liability": a.liability, "fee_pool": a.fee_pool, "unpaid_total": a.unpaid_total},
        "conservation": a.credited_total == a.liability + a.fee_pool + a.unpaid_total}));
}

/// Credits a deposit to any session key, as the gateway's attestor would (for the web page e2e).
fn credit(session: Pubkey, amount: u64) {
    let relayer = kp("payer");
    let args = zol_ledger::CreditArgs {
        deposit_id: rand::random(),
        amount,
        session_key: session,
        payout_commitment: [7; 32],
        flags: 0,
        pot_id: [0; 32],
        app_data: vec![],
    };
    let digest = CreditAttestation {
        network: NETWORK,
        app: APPP.to_bytes(),
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
            deposit: pda(&[seeds::DEPOSIT, &args.deposit_id], &LEDGER),
            balance: balance_pda(&session),
            instructions: IX_SYSVAR,
            system_program: SYSTEM,
        },
        zol_ledger::instruction::CreditDeposit { args },
    );
    step!(format!("credit_deposit {session}"), send(BASE, &[ed25519_ix(&attestor(), &digest), ix], &[&relayer]));
}

/// Writes genesis accounts for an already-active coinflip app (Config + App), as
/// gateway/tests/multihost.rs does, so a run with the real gateway skips the 3600 s
/// activation delay. The vault and genesis head come from zol-zcashd.
/// usage: preload <out_dir> <attestor_group_key_b58> <vault_hex43> <genesis_cmx_hex32> <guardian_b58>...
fn preload(args: &[String]) {
    use anchor_lang::{AccountSerialize, Space};
    let out = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(out).unwrap();
    let attestor: Pubkey = args[3].parse().unwrap();
    let vault: [u8; 43] = hex_bytes(&args[4]).try_into().unwrap();
    let genesis: [u8; 32] = hex_bytes(&args[5]).try_into().unwrap();
    let guardians: Vec<Pubkey> = args[6..].iter().map(|g| g.parse().unwrap()).collect();
    let admin = kp("payer").pubkey();
    let mut gk = [Pubkey::default(); 7];
    gk[..guardians.len()].copy_from_slice(&guardians);
    let mut p = params();
    p.network_fee_allowance = 20_000; // ZIP 317 fee of the smallest padded transition
    let config = st::Config {
        admin,
        attestor,
        guardians: gk,
        guardian_count: guardians.len() as u8,
        guardian_threshold: 2,
        network: zol_wire::Network::Regtest as u8,
        paused: false,
        params: p,
        bump: Pubkey::find_program_address(&[seeds::CONFIG], &LEDGER).1,
    };
    let app = st::App {
        program_id: APPP,
        authority: app_authority(),
        registrant: admin,
        state: st::AppState::Active,
        activates_at: 0,
        vault_address: vault,
        genesis_head: genesis,
        deposit_cap: u64::MAX / 4,
        liability: 0,
        deposit_count: 0,
        credited_total: 0,
        fee_pool: 0,
        next_settlement: 0,
        paid_count: 0,
        unpaid_total: 0,
        bond: 0,
        bump: Pubkey::find_program_address(&[seeds::APP, APPP.as_ref()], &LEDGER).1,
    };
    let write = |name: &str, key: Pubkey, data: Vec<u8>| {
        use base64::Engine;
        let v = json!({"pubkey": key.to_string(), "account": {"lamports": 1_000_000_000u64,
            "data": [base64::engine::general_purpose::STANDARD.encode(&data), "base64"],
            "owner": LEDGER.to_string(), "executable": false, "rentEpoch": 0, "space": data.len()}});
        let path = out.join(format!("{name}.json"));
        std::fs::write(&path, v.to_string()).unwrap();
        println!("{key} {}", path.display());
    };
    let ser = |a: &dyn Fn(&mut Vec<u8>), space: usize| {
        let mut d = Vec::new();
        a(&mut d);
        d.resize(space, 0);
        d
    };
    write("config", config_pda(), ser(&|d| config.try_serialize(d).unwrap(), 8 + st::Config::INIT_SPACE));
    write("app", app_pda(), ser(&|d| app.try_serialize(d).unwrap(), 8 + st::App::INIT_SPACE));
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// `settle` for exiting balances (anyone may call it). usage: settle <session_b58>...
fn settle_cmd(sessions: &[String]) {
    let relayer = kp("payer");
    let a: st::App = load(BASE, &app_pda());
    let mut ix = ledger_ix(
        zol_ledger::accounts::Settle { payer: relayer.pubkey(), config: config_pda(), app: app_pda(), settlement: settlement_pda(a.next_settlement), system_program: SYSTEM },
        zol_ledger::instruction::Settle {},
    );
    for s in sessions {
        ix.accounts.push(AccountMeta::new(balance_pda(&s.parse().unwrap()), false));
    }
    step!("settle", send(BASE, &[ix], &[&relayer]));
    let s: st::Settlement = load(BASE, &settlement_pda(a.next_settlement));
    println!("{}", json!({"settlement": a.next_settlement, "settlement_pda": settlement_pda(a.next_settlement).to_string(),
        "payouts": s.payouts[..s.count as usize].iter().map(|p| p.amount).collect::<Vec<_>>(), "total": s.total}));
}

/// Prints the settlement state (Unpaid/Paid). usage: settlement <seq>
fn settlement_cmd(seq: u64) {
    let s: Option<st::Settlement> = account(BASE, &settlement_pda(seq)).and_then(|a| anchor_lang::AccountDeserialize::try_deserialize(&mut &a.1[..]).ok());
    let a: st::App = load(BASE, &app_pda());
    println!("{}", json!({"seq": seq, "state": s.map(|s| format!("{:?}", s.state)), "app": {"paid_count": a.paid_count, "unpaid_total": a.unpaid_total, "fee_pool": a.fee_pool, "liability": a.liability, "credited_total": a.credited_total, "deposit_count": a.deposit_count}}));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("setup") => setup(),
        Some("play") => play(),
        Some("preload") => preload(&args),
        Some("settle") => settle_cmd(&args[2..]),
        Some("settlement") => settlement_cmd(args[2].parse().unwrap()),
        Some("credit") => credit(args[2].parse().unwrap(), args.get(3).map(|a| a.parse().unwrap()).unwrap_or(1_000_000)),
        _ => eprintln!("usage: coinflip setup|play|credit <session> [amount]"),
    }
}

/// `ZOL_DEV_TIMINGS=1` with a ledger from target/deploy-dev (`dev-timings` build):
/// 5 s activation delay and 10 s refund grace instead of an hour.
fn dev_timings() -> bool {
    std::env::var("ZOL_DEV_TIMINGS").is_ok_and(|v| v == "1")
}
