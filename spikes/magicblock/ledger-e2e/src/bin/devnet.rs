//! Ledger administration on a public cluster (devnet) for the testnet trial: any app.
//!
//!   ZOL_RPC=https://api.devnet.solana.com   (default)
//!   devnet init          --admin <keypair.json> --attestor <b58> --guardians <b58,b58,b58>
//!   devnet set-min-exit  --admin <keypair.json> --zat <n>     (the live params, min_exit changed)
//!   devnet set-max-seats --admin <keypair.json> --seats <n>   (the live params, max_seats changed)
//!   devnet status       [--app <program b58>]
//!   devnet register      --app <program b58> --payer <keypair.json>
//!   devnet activate      --app <program b58> --admin <keypair.json> --vault <hex43> --genesis <hex32>
//!   devnet set-cap       --app <program b58> --admin <keypair.json> --cap <zat>
//!   devnet settle        --app <program b58> --payer <keypair.json> <session b58>...   (balances must have requested exit)
//!   devnet settlement    --app <program b58> <seq>
//!   devnet unfreeze      --app <program b58> --payer <keypair.json> --guardians <g1.json,g2.json>   (a threshold of guardians)
//!
//! Every app command names the app (`--app`, its program id), so a cap or an activation never
//! lands on the wrong one. `register` sends the app's own `register` instruction, so it works for
//! any app whose `register` follows the template in dev-docs/build-an-app.md (that name, no
//! arguments, the same accounts); other apps send theirs with their own client. `status --app`
//! prints the app id in hex, which the operators' configs use. Onboarding is admin-run:
//! docs/ops/onboard-app.md.
//!
//! Testnet parameters: production timings (1 h activation delay, 3,900 s refund grace),
//! denominations 0.01 and 0.1 TAZ. Activate only once the genesis head is at full depth
//! (`depths.full`: 60 blocks on the trial, about 25 minutes): guardians cancel an activation whose
//! head is shallower.
#[path = "../common.rs"]
#[allow(dead_code)]
mod common;

use anchor_lang::{InstructionData, ToAccountMetas};
use common::*;
use serde_json::json;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use zol_ledger::state::{self as st, seeds, Params};

const LEDGER: Pubkey = zol_ledger::ID;
const SYSTEM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");
const BPF_UPGRADEABLE: Pubkey = solana_pubkey::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");

fn params() -> Params {
    Params {
        entry_fee_bps: 30,
        exit_fee_bps: 30,
        // At least 5,000 x the smallest padded action size (4): see docs/ops/multihost.md.
        network_fee_allowance: 20_000,
        // The trial's live values (set after its init with set-min-exit and set-max-seats).
        min_exit: 50_000,
        max_seats: 16,
        max_open_secs: 3_600,
        min_session_secs: 60,
        max_session_secs: 7_200,
        refund_grace_secs: 3_900,
        activation_delay_secs: 3_600,
        registration_bond: 0,
        denominations: [1_000_000, 10_000_000, 0, 0],
    }
}

fn url() -> String {
    std::env::var("ZOL_RPC").unwrap_or_else(|_| "https://api.devnet.solana.com".into())
}
fn arg(name: &str) -> String {
    opt(name).unwrap_or_else(|| panic!("missing {name}"))
}
fn keypair(path: &str) -> Keypair {
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    Keypair::try_from(&bytes[..]).unwrap()
}
fn pda(s: &[&[u8]], p: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(s, p).0
}
fn config_pda() -> Pubkey {
    pda(&[seeds::CONFIG], &LEDGER)
}
fn opt(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}
/// The app's program id, from `--app`.
fn app_program() -> Pubkey {
    arg("--app").parse().expect("--app: a base58 program id")
}
fn app_pda() -> Pubkey {
    pda(&[seeds::APP, app_program().as_ref()], &LEDGER)
}
fn ledger_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id: LEDGER, accounts: accounts.to_account_metas(None), data: data.data() }
}
fn hexn<const N: usize>(s: &str) -> [u8; N] {
    hex::decode(s).unwrap().try_into().unwrap_or_else(|_| panic!("expected {N} bytes"))
}
fn run(name: &str, ixs: &[Instruction], signers: &[&Keypair]) {
    match send(&url(), ixs, signers) {
        Ok(s) => println!("{}", json!({"step": name, "ok": true, "sig": s.sig})),
        Err(f) => {
            println!("{}", json!({"step": name, "ok": false, "err": f.err, "logs": f.logs}));
            std::process::exit(1);
        }
    }
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "init" => {
            let admin = keypair(&arg("--admin"));
            let attestor: Pubkey = arg("--attestor").parse().unwrap();
            let guardians: Vec<Pubkey> = arg("--guardians").split(',').map(|g| g.parse().unwrap()).collect();
            let ix = ledger_ix(
                zol_ledger::accounts::Initialize {
                    payer: admin.pubkey(),
                    config: config_pda(),
                    program: LEDGER,
                    program_data: pda(&[LEDGER.as_ref()], &BPF_UPGRADEABLE),
                    system_program: SYSTEM,
                },
                zol_ledger::instruction::Initialize {
                    args: zol_ledger::InitializeArgs {
                        admin: admin.pubkey(),
                        attestor,
                        guardians,
                        guardian_threshold: 2,
                        network: zol_wire::Network::Testnet as u8,
                        params: params(),
                    },
                },
            );
            run("initialize", &[ix], &[&admin]);
        }
        "register" => {
            let payer = keypair(&arg("--payer"));
            let program = app_program();
            // The template's `register`: the coin flip's accounts and data with another program id.
            // Anchor's discriminator depends only on the instruction's name.
            let ix = Instruction {
                program_id: program,
                accounts: zol_coinflip::accounts::Register {
                    payer: payer.pubkey(),
                    this_program: program,
                    app_authority: pda(&[seeds::APP_AUTHORITY], &program),
                    config: config_pda(),
                    app: app_pda(),
                    ledger_program: LEDGER,
                    system_program: SYSTEM,
                }
                .to_account_metas(None),
                data: zol_coinflip::instruction::Register {}.data(),
            };
            run("register", &[ix], &[&payer]);
        }
        "activate" => {
            let admin = keypair(&arg("--admin"));
            let ix = ledger_ix(
                zol_ledger::accounts::ActivateApp { admin: admin.pubkey(), config: config_pda(), app: app_pda() },
                zol_ledger::instruction::ActivateApp { vault_address: hexn::<43>(&arg("--vault")), genesis_head: hexn::<32>(&arg("--genesis")) },
            );
            run("activate_app", &[ix], &[&admin]);
            println!("{}", json!({"chain_now": now_on_chain(&url()), "accepting_after": now_on_chain(&url()) + params().activation_delay_secs}));
        }
        "set-cap" => {
            let admin = keypair(&arg("--admin"));
            let cap: u64 = arg("--cap").parse().unwrap();
            let ix = ledger_ix(
                zol_ledger::accounts::SetCap { admin: admin.pubkey(), config: config_pda(), app: app_pda() },
                zol_ledger::instruction::SetCap { cap },
            );
            run("set_cap", &[ix], &[&admin]);
        }
        "set-min-exit" => {
            let admin = keypair(&arg("--admin"));
            let mut p = load::<st::Config>(&url(), &config_pda()).params;
            p.min_exit = arg("--zat").parse().unwrap();
            let ix = ledger_ix(
                zol_ledger::accounts::AdminOnly { admin: admin.pubkey(), config: config_pda() },
                zol_ledger::instruction::SetConfig { args: zol_ledger::SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None } },
            );
            run("set_config(min_exit)", &[ix], &[&admin]);
        }
        "set-max-seats" => {
            let admin = keypair(&arg("--admin"));
            let mut p = load::<st::Config>(&url(), &config_pda()).params;
            p.max_seats = arg("--seats").parse().unwrap();
            let ix = ledger_ix(
                zol_ledger::accounts::AdminOnly { admin: admin.pubkey(), config: config_pda() },
                zol_ledger::instruction::SetConfig { args: zol_ledger::SetConfigArgs { new_admin: None, attestor: None, params: Some(p), paused: None } },
            );
            run("set_config(max_seats)", &[ix], &[&admin]);
        }
        "settle" => {
            let payer = keypair(&arg("--payer"));
            let a: st::App = load(&url(), &app_pda());
            let seq = a.next_settlement;
            let settlement = pda(&[seeds::SETTLEMENT, app_pda().as_ref(), &seq.to_le_bytes()], &LEDGER);
            let mut ix = ledger_ix(
                zol_ledger::accounts::Settle { payer: payer.pubkey(), config: config_pda(), app: app_pda(), settlement, system_program: SYSTEM },
                zol_ledger::instruction::Settle {},
            );
            let args: Vec<String> = std::env::args().collect();
            let flags = ["--payer", "--app"];
            let sessions = (2..args.len()).filter(|&i| !args[i].starts_with("--") && !flags.contains(&args[i - 1].as_str()));
            for i in sessions {
                let session: Pubkey = args[i].parse().expect("a session key (base58)");
                ix.accounts.push(solana_instruction::AccountMeta::new(pda(&[seeds::BALANCE, app_pda().as_ref(), session.as_ref()], &LEDGER), false));
            }
            run("settle", &[ix], &[&payer]);
            let s: st::Settlement = load(&url(), &settlement);
            println!("{}", json!({"settlement": seq, "payouts": s.payouts[..s.count as usize].iter().map(|p| p.amount).collect::<Vec<_>>(), "total": s.total}));
        }
        "settlement" => {
            let a: Vec<String> = std::env::args().collect();
            let seq: u64 = a.iter().skip(2).filter(|x| !x.starts_with("--")).find(|x| x.parse::<u64>().is_ok()).expect("a settlement number").parse().unwrap();
            let s: st::Settlement = load(&url(), &pda(&[seeds::SETTLEMENT, app_pda().as_ref(), &seq.to_le_bytes()], &LEDGER));
            let a: st::App = load(&url(), &app_pda());
            println!("{}", json!({"seq": seq, "state": format!("{:?}", s.state), "total": s.total,
                "app": {"credited_total": a.credited_total, "liability": a.liability, "fee_pool": a.fee_pool, "unpaid_total": a.unpaid_total, "paid_count": a.paid_count}}));
        }
        "unfreeze" => {
            let payer = keypair(&arg("--payer"));
            let guardians: Vec<Keypair> = arg("--guardians").split(',').map(keypair).collect();
            let mut ix = ledger_ix(zol_ledger::accounts::UnfreezeApp { config: config_pda(), app: app_pda() }, zol_ledger::instruction::UnfreezeApp {});
            for g in &guardians {
                ix.accounts.push(solana_instruction::AccountMeta::new_readonly(g.pubkey(), true));
            }
            let mut signers: Vec<&Keypair> = vec![&payer];
            signers.extend(guardians.iter());
            run("unfreeze_app", &[ix], &signers);
        }
        "status" => {
            let u = url();
            match account(&u, &config_pda()) {
                None => println!("{}", json!({"config": null})),
                Some(_) => {
                    let c: st::Config = load(&u, &config_pda());
                    println!(
                        "{}",
                        json!({"config": config_pda().to_string(), "admin": c.admin.to_string(), "attestor": c.attestor.to_string(),
                               "guardians": c.guardians[..c.guardian_count as usize].iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                               "network": c.network, "paused": c.paused, "denominations": c.params.denominations, "min_exit": c.params.min_exit, "max_seats": c.params.max_seats})
                    );
                }
            }
            if opt("--app").is_none() {
                return;
            }
            let program = app_program();
            match account(&u, &app_pda()) {
                None => println!("{}", json!({"program": program.to_string(), "app_id_hex": hex::encode(program), "app": null})),
                Some(_) => {
                    let a: st::App = load(&u, &app_pda());
                    println!(
                        "{}",
                        json!({"program": program.to_string(), "app_id_hex": hex::encode(program), "app": app_pda().to_string(),
                               "state": format!("{:?}", a.state), "activates_at": a.activates_at,
                               "chain_now": now_on_chain(&u), "deposit_cap": a.deposit_cap, "vault": hex::encode(a.vault_address),
                               "genesis_head": hex::encode(a.genesis_head), "credited_total": a.credited_total, "liability": a.liability})
                    );
                }
            }
        }
        _ => {
            eprintln!("usage: devnet init|set-min-exit|set-max-seats|status|register|activate|set-cap|settle|settlement|unfreeze (see the file header)");
            std::process::exit(2);
        }
    }
}
