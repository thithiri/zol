//! Property test (plan task 2.11): random instruction sequences, honest or not,
//! never break the ledger invariants.
//!
//!   sum(Balance) + sum(live PotReceipt.total) == App.liability
//!   App.credited_total == sum(credited Deposit.amount)
//!   sum(Pot.amounts) == PotReceipt.total at cash_out (else the pot is refused)
//!   credited_total == liability + fee_pool + sum(settlements)

use proptest::prelude::*;
use solana_keypair::Keypair;
use solana_signer::Signer;
use zol_ledger::state::PotState;
use zol_tests::*;

static STATS: std::sync::Mutex<std::collections::BTreeMap<String, usize>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

const PLAYERS: usize = 4;
const POTS: usize = 3;

#[derive(Clone, Debug)]
enum Op {
    Credit { p: usize, amount: u64 },
    OpenPot { pot: usize },
    BuyIn { pot: usize, p: usize, amount: u64 },
    Leave { pot: usize, p: usize },
    Delegate { pot: usize, honest: bool },
    Return { pot: usize, mode: u8, from: usize, to: usize, frac: u8 },
    CashOut { pot: usize },
    Refund { pot: usize },
    Warp { secs: i64 },
    Exit { p: usize },
    Settle { mask: u8 },
    MarkPaid,
    Freeze,
    Unfreeze,
}

fn op() -> impl Strategy<Value = Op> {
    let p = 0..PLAYERS;
    let pot = 0..POTS;
    prop_oneof![
        4 => (p.clone(), prop::sample::select(vec![100_000u64, 500_000, 1_000_000, 7_777_777])).prop_map(|(p, amount)| Op::Credit { p, amount }),
        2 => pot.clone().prop_map(|pot| Op::OpenPot { pot }),
        5 => (pot.clone(), p.clone(), 1u64..400_000).prop_map(|(pot, p, amount)| Op::BuyIn { pot, p, amount }),
        1 => (pot.clone(), p.clone()).prop_map(|(pot, p)| Op::Leave { pot, p }),
        4 => (pot.clone(), prop::bool::weighted(0.85)).prop_map(|(pot, honest)| Op::Delegate { pot, honest }),
        5 => (pot.clone(), 0u8..4, 0usize..8, 0usize..8, any::<u8>()).prop_map(|(pot, mode, from, to, frac)| Op::Return { pot, mode, from, to, frac }),
        4 => pot.clone().prop_map(|pot| Op::CashOut { pot }),
        2 => pot.clone().prop_map(|pot| Op::Refund { pot }),
        2 => prop::sample::select(vec![30i64, 120, 300, 700, 1_600, 5_000]).prop_map(|secs| Op::Warp { secs }),
        2 => p.clone().prop_map(|p| Op::Exit { p }),
        3 => any::<u8>().prop_map(|mask| Op::Settle { mask }),
        3 => Just(Op::MarkPaid),
        1 => Just(Op::Freeze),
        3 => Just(Op::Unfreeze),
    ]
}

struct World {
    env: Env,
    players: Vec<Keypair>,
    next_deposit: u8,
    paid: u64,
}

impl World {
    fn pot_id(i: usize) -> [u8; 32] {
        [0xC0 + i as u8; 32]
    }

    fn receipt(&self, pot: usize) -> Option<st::PotReceipt> {
        self.env.try_account(&receipt_pda(&pot_pda(&app_pda(&TEST_APP), &Self::pot_id(pot))))
    }

    fn seat_players(&self, pot: usize) -> Vec<solana_pubkey::Pubkey> {
        self.receipt(pot).map(|r| r.players[..r.seat_count as usize].to_vec()).unwrap_or_default()
    }

    fn apply(&mut self, op: &Op) {
        let name = format!("{op:?}").split([' ', '{']).next().unwrap().to_string();
        let res: Result<(), ()> = match op.clone() {
            Op::Credit { p, amount } => {
                self.next_deposit = self.next_deposit.wrapping_add(1);
                let mut id = [0u8; 32];
                id[0] = self.next_deposit;
                id[1] = 0x77;
                self.env.credit(id, amount, self.players[p].pubkey(), [p as u8; 32]).map(|_| ()).map_err(|_| ())
            }
            Op::OpenPot { pot } => self.env.open_pot(Self::pot_id(pot), 600).map(|_| ()).map_err(|_| ()),
            Op::BuyIn { pot, p, amount } => {
                let k = self.players[p].insecure_clone();
                self.env.buy_in(Self::pot_id(pot), &k, amount).map(|_| ()).map_err(|_| ())
            }
            Op::Leave { pot, p } => {
                let k = self.players[p].insecure_clone();
                self.env.leave_pot(Self::pot_id(pot), &k).map(|_| ()).map_err(|_| ())
            }
            Op::Delegate { pot, honest } => {
                if self.receipt(pot).is_none() {
                    return;
                }
                let mut e = self.env.expected(Self::pot_id(pot));
                if !honest && !e.is_empty() {
                    e[0].amount += 1;
                }
                self.env.delegate(Self::pot_id(pot), e, 900).map(|_| ()).map_err(|_| ())
            }
            Op::Return { pot, mode, from, to, frac } => {
                // Only a pot that is out can come back.
                let id = Self::pot_id(pot);
                let key = pot_pda(&app_pda(&TEST_APP), &id);
                if self.env.raw(&key).map(|a| a.owner) != Some(DLP) {
                    return;
                }
                self.env.simulate_return(id, |p| {
                    let n = p.seat_count as usize;
                    match mode {
                        // Honest result: move a fraction of one seat to another.
                        0 | 1 if n > 0 => {
                            let (f, t) = (from % n, to % n);
                            let amt = p.amounts[f] / 255 * frac as u64;
                            p.amounts[f] -= amt;
                            p.amounts[t] += amt;
                        }
                        // Faulty validator: create value or add a seat.
                        2 => p.amounts[from % 8] = p.amounts[from % 8].wrapping_add(1 + frac as u64),
                        _ => p.seat_count = p.seat_count.wrapping_add(1),
                    }
                    p.delegated = false;
                });
                Ok(())
            }
            Op::CashOut { pot } => {
                let players = self.seat_players(pot);
                self.env.cash_out(Self::pot_id(pot), &players).map(|_| ()).map_err(|_| ())
            }
            Op::Refund { pot } => {
                let players = self.seat_players(pot);
                self.env.refund_pot(Self::pot_id(pot), &players).map(|_| ()).map_err(|_| ())
            }
            Op::Warp { secs } => {
                self.env.warp(secs);
                Ok(())
            }
            Op::Exit { p } => {
                let k = self.players[p].insecure_clone();
                self.env.request_exit(&k).map(|_| ()).map_err(|_| ())
            }
            Op::Settle { mask } => {
                let ps: Vec<_> = (0..PLAYERS).filter(|i| mask & (1 << i) != 0).map(|i| self.players[i].pubkey()).collect();
                self.env.settle(&ps).map(|_| ()).map_err(|_| ())
            }
            Op::MarkPaid => {
                let seq = self.env.app_state().paid_count;
                let r = self.env.mark_paid(seq).map(|_| ()).map_err(|_| ());
                if r.is_ok() {
                    self.paid += 1;
                }
                r
            }
            Op::Freeze => self.env.freeze(0).map(|_| ()).map_err(|_| ()),
            Op::Unfreeze => {
                let g0 = self.env.guardians[0].insecure_clone();
                let g1 = self.env.guardians[1].insecure_clone();
                self.env.unfreeze(&[&g0, &g1]).map(|_| ()).map_err(|_| ())
            }
        };
        if res.is_ok() {
            *STATS.lock().unwrap().entry(name).or_insert(0) += 1;
        }
    }

    fn check(&self) {
        self.env.check_invariants();
        // A pot with a terminal receipt is never live again; paid count is monotone.
        for pot in 0..POTS {
            if let Some(r) = self.receipt(pot) {
                if matches!(r.state, PotState::CashedOut | PotState::Refunded) {
                    assert!(!r.is_live());
                }
            }
        }
        assert_eq!(self.env.app_state().paid_count, self.paid);
    }
}

proptest! {
    // 64 cases by default (fast tier); PROPTEST_CASES=2000 in the full tier.
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64),
        max_shrink_iters: 200,
        ..ProptestConfig::default()
    })]

    #[test]
    fn invariants_hold_under_random_sequences(ops in prop::collection::vec(op(), 20..120)) {
        let mut env = Env::new();
        let players: Vec<Keypair> = (0..PLAYERS).map(|_| Keypair::new()).collect();
        for p in &players {
            env.svm.airdrop(&p.pubkey(), 10_000_000_000).unwrap();
        }
        let mut w = World { env, players, next_deposit: 0, paid: 0 };
        for p in 0..PLAYERS {
            w.apply(&Op::Credit { p, amount: 1_000_000 });
        }
        for op in &ops {
            w.apply(op);
            w.check();
        }
        if std::env::var("ZOL_STATS").is_ok() {
            eprintln!("successful ops so far: {:?}", STATS.lock().unwrap());
        }
    }
}

#[test]
fn world_can_reach_every_state() {
    let mut env = Env::new();
    let players: Vec<Keypair> = (0..PLAYERS).map(|_| Keypair::new()).collect();
    for p in &players {
        env.svm.airdrop(&p.pubkey(), 10_000_000_000).unwrap();
    }
    let mut w = World { env, players, next_deposit: 0, paid: 0 };
    let script = [
        Op::Credit { p: 0, amount: 1_000_000 },
        Op::Credit { p: 1, amount: 1_000_000 },
        Op::OpenPot { pot: 0 },
        Op::BuyIn { pot: 0, p: 0, amount: 100_000 },
        Op::BuyIn { pot: 0, p: 1, amount: 100_000 },
        Op::Delegate { pot: 0, honest: true },
        Op::Return { pot: 0, mode: 0, from: 0, to: 1, frac: 128 },
        Op::CashOut { pot: 0 },
        Op::Exit { p: 0 },
        Op::Settle { mask: 1 },
        Op::MarkPaid,
    ];
    for op in &script {
        w.apply(op);
        w.check();
    }
    let stats = STATS.lock().unwrap().clone();
    for k in ["Delegate", "Return", "CashOut", "Settle", "MarkPaid"] {
        assert!(stats.get(k).copied().unwrap_or(0) >= 1, "{k} never succeeded: {stats:?}");
    }
}
