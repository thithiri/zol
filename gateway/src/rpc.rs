//! `Ledger` over Solana JSON-RPC (plan 3.6, the relayer).
//!
//! Reads use the `finalized` commitment (signers act only on finalized state).
//! Writes are relayed by a fee-paying relayer key; attestations ride in an
//! Ed25519 precompile instruction placed right before the ledger instruction.

use std::time::{Duration, Instant};

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AccountDeserialize, Discriminator, InstructionData, ToAccountMetas};
use base64::Engine;
use serde_json::{json, Value};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;
use zol_ledger::state::{seeds, App, Balance, Config, Deposit, Settlement};
use zol_ledger::CreditArgs;
use zol_wire::digest::{paid_digest, void_digest, Sha256};
use zol_wire::CreditAttestation;

use crate::ledger::{Ledger, LedgerError, Result};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const IX_SYSVAR: Pubkey = anchor_lang::pubkey!("Sysvar1nstructions1111111111111111111111111");
const SYSTEM: Pubkey = anchor_lang::pubkey!("11111111111111111111111111111111");
/// Byte offset of `Deposit.app` (discriminator 8, id 32, state 1).
const DEPOSIT_APP_OFFSET: usize = 8 + 32 + 1;
/// Byte offset of `Settlement.app`.
const SETTLEMENT_APP_OFFSET: usize = 8;
/// An answer whose slot is this far behind the newest slot this client has seen comes
/// from a lagging RPC backend and is refused (about 2 min). Load-balanced providers were
/// seen serving one backend stuck ~70 min behind (Helius devnet, 2026-10-11).
pub const MAX_SLOT_LAG: u64 = 300;

pub struct RpcLedger {
    url: String,
    http: reqwest::blocking::Client,
    relayer: Keypair,
    /// The attestor group key named in the ledger config.
    attestor: [u8; 32],
    network: u8,
    /// Commitment a sent transaction must reach before `submit_*` returns.
    pub send_commitment: &'static str,
    pub timeout: Duration,
    /// Newest slot seen in any answer: older answers are refused (`MAX_SLOT_LAG`).
    seen_slot: std::sync::atomic::AtomicU64,
}

fn rpc_err(e: impl std::fmt::Display) -> LedgerError {
    LedgerError::Rpc(e.to_string())
}

impl RpcLedger {
    pub fn new(url: &str, relayer: Keypair, attestor: [u8; 32], network: u8) -> Self {
        Self {
            url: url.to_string(),
            http: reqwest::blocking::Client::new(),
            relayer,
            attestor,
            network,
            send_commitment: "finalized",
            timeout: Duration::from_secs(90),
            seen_slot: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn relayer_pubkey(&self) -> Pubkey {
        self.relayer.pubkey()
    }

    /// One JSON-RPC call. Rate limiting (HTTP 429, or a JSON-RPC error with
    /// code 429, as public endpoints send when several gateways share an IP) is
    /// retried with growing waits, about 7.5 s in all, before it fails the tick.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut wait = Duration::from_millis(500);
        for attempt in 0..5 {
            let r = self.http.post(&self.url).json(&body).send().map_err(rpc_err)?;
            let limited_http = r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS;
            let v: Option<Value> = r.json().ok();
            let limited = limited_http || v.as_ref().is_some_and(|v| v["error"]["code"] == 429);
            if limited && attempt < 4 {
                std::thread::sleep(wait);
                wait *= 2;
                continue;
            }
            let v = v.ok_or_else(|| rpc_err(format!("{method}: response is not JSON")))?;
            if let Some(e) = v.get("error") {
                return Err(LedgerError::Rpc(format!("{method}: {e}")));
            }
            return Ok(v["result"].clone());
        }
        unreachable!("the last attempt returns")
    }

    /// Refuses an answer from a backend that is behind what this client already saw.
    fn fresh(&self, method: &str, slot: Option<u64>) -> Result<()> {
        let Some(slot) = slot else { return Err(rpc_err(format!("{method}: answer has no context slot"))) };
        let newest = self.seen_slot.fetch_max(slot, std::sync::atomic::Ordering::SeqCst).max(slot);
        if slot + MAX_SLOT_LAG < newest {
            return Err(rpc_err(format!("{method}: stale answer from a lagging RPC backend (slot {slot}, {} behind)", newest - slot)));
        }
        Ok(())
    }

    fn account_data(&self, key: &Pubkey) -> Result<Option<Vec<u8>>> {
        let r = self.call("getAccountInfo", json!([key.to_string(), {"encoding": "base64", "commitment": "finalized"}]))?;
        self.fresh("getAccountInfo", r["context"]["slot"].as_u64())?;
        if r["value"].is_null() {
            return Ok(None);
        }
        let b64 = r["value"]["data"][0].as_str().ok_or_else(|| rpc_err("no data"))?;
        Ok(Some(B64.decode(b64).map_err(rpc_err)?))
    }

    fn typed<T: AccountDeserialize>(&self, key: &Pubkey) -> Result<Option<T>> {
        match self.account_data(key)? {
            None => Ok(None),
            Some(d) => T::try_deserialize(&mut &d[..]).map(Some).map_err(rpc_err),
        }
    }

    fn program_accounts<T: AccountDeserialize + Discriminator>(&self, app_offset: usize, app: &Pubkey) -> Result<Vec<T>> {
        let r = self.call(
            "getProgramAccounts",
            json!([zol_ledger::ID.to_string(), {
                "encoding": "base64",
                "commitment": "finalized",
                "withContext": true,
                "filters": [
                    {"memcmp": {"offset": 0, "bytes": bs58::encode(T::DISCRIMINATOR).into_string()}},
                    {"memcmp": {"offset": app_offset, "bytes": app.to_string()}}
                ]
            }]),
        )?;
        self.fresh("getProgramAccounts", r["context"]["slot"].as_u64())?;
        let mut out = Vec::new();
        for a in r["value"].as_array().ok_or_else(|| rpc_err("not an array"))? {
            let d = B64.decode(a["account"]["data"][0].as_str().unwrap_or_default()).map_err(rpc_err)?;
            out.push(T::try_deserialize(&mut &d[..]).map_err(rpc_err)?);
        }
        Ok(out)
    }

    /// Signs with the relayer (fee payer) and `extra`, sends, and waits for `send_commitment`.
    pub fn send(&self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<String> {
        let bh = self.call("getLatestBlockhash", json!([{"commitment": "confirmed"}]))?;
        let hash: solana_hash::Hash =
            bh["value"]["blockhash"].as_str().ok_or_else(|| rpc_err("no blockhash"))?.parse().map_err(rpc_err)?;
        let mut signers: Vec<&Keypair> = vec![&self.relayer];
        signers.extend_from_slice(extra);
        let tx = Transaction::new(&signers, Message::new(ixs, Some(&self.relayer.pubkey())), hash);
        let wire = B64.encode(bincode::serialize(&tx).map_err(rpc_err)?);
        let sig = self.call("sendTransaction", json!([wire, {"encoding": "base64", "preflightCommitment": "confirmed"}]))?;
        let sig = sig.as_str().ok_or_else(|| rpc_err("no signature"))?.to_string();
        let start = Instant::now();
        loop {
            let st = self.call("getSignatureStatuses", json!([[sig], {"searchTransactionHistory": true}]))?;
            let s = &st["value"][0];
            if !s.is_null() {
                if !s["err"].is_null() {
                    return Err(LedgerError::Tx(format!("{sig}: {}", s["err"])));
                }
                let level = s["confirmationStatus"].as_str().unwrap_or("");
                let done = match self.send_commitment {
                    "processed" => true,
                    "confirmed" => level == "confirmed" || level == "finalized",
                    _ => level == "finalized",
                };
                if done {
                    return Ok(sig);
                }
            }
            if start.elapsed() > self.timeout {
                return Err(LedgerError::Tx(format!("{sig}: not {} in time", self.send_commitment)));
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    fn precompile(&self, digest: &[u8; 32], sig: &[u8; 64]) -> Instruction {
        let ix = solana_ed25519_program::new_ed25519_instruction_with_signature(digest, sig, &self.attestor);
        Instruction { program_id: ix.program_id, accounts: vec![], data: ix.data }
    }

    fn pda(s: &[&[u8]]) -> Pubkey {
        Pubkey::find_program_address(s, &zol_ledger::ID).0
    }

    fn ledger_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
        Instruction { program_id: zol_ledger::ID, accounts: accounts.to_account_metas(None), data: data.data() }
    }
}

impl Ledger for RpcLedger {
    fn config(&self) -> Result<Config> {
        self.typed(&Self::pda(&[seeds::CONFIG]))?.ok_or_else(|| rpc_err("no config"))
    }
    fn app(&self, p: &Pubkey) -> Result<Option<App>> {
        self.typed(&Self::pda(&[seeds::APP, p.as_ref()]))
    }
    fn deposit(&self, id: &[u8; 32]) -> Result<Option<Deposit>> {
        self.typed(&Self::pda(&[seeds::DEPOSIT, id]))
    }
    fn deposits(&self, p: &Pubkey) -> Result<Vec<([u8; 32], Deposit)>> {
        let app = Self::pda(&[seeds::APP, p.as_ref()]);
        Ok(self.program_accounts::<Deposit>(DEPOSIT_APP_OFFSET, &app)?.into_iter().map(|d| (d.id, d)).collect())
    }
    fn balance(&self, p: &Pubkey, s: &Pubkey) -> Result<Option<Balance>> {
        let app = Self::pda(&[seeds::APP, p.as_ref()]);
        self.typed(&Self::pda(&[seeds::BALANCE, app.as_ref(), s.as_ref()]))
    }
    fn settlements(&self, p: &Pubkey) -> Result<Vec<Settlement>> {
        let app = Self::pda(&[seeds::APP, p.as_ref()]);
        self.program_accounts::<Settlement>(SETTLEMENT_APP_OFFSET, &app)
    }
    fn now(&self) -> Result<i64> {
        let slot = self.call("getSlot", json!([{"commitment": "finalized"}]))?;
        self.fresh("getSlot", slot.as_u64())?;
        let t = self.call("getBlockTime", json!([slot]))?;
        t.as_i64().ok_or_else(|| rpc_err("no block time"))
    }

    fn submit_credit(&mut self, p: &Pubkey, args: CreditArgs, sig: [u8; 64]) -> Result<()> {
        let digest = CreditAttestation {
            network: self.network,
            app: p.to_bytes(),
            deposit_id: args.deposit_id,
            amount: args.amount,
            session_key: args.session_key.to_bytes(),
            payout_commitment: args.payout_commitment,
            flags: args.flags,
            pot_id: args.pot_id,
            app_data: &args.app_data,
        }
        .digest::<Sha256>();
        let app = Self::pda(&[seeds::APP, p.as_ref()]);
        let ix = Self::ledger_ix(
            zol_ledger::accounts::CreditDeposit {
                relayer: self.relayer.pubkey(),
                config: Self::pda(&[seeds::CONFIG]),
                app,
                deposit: Self::pda(&[seeds::DEPOSIT, &args.deposit_id]),
                balance: Self::pda(&[seeds::BALANCE, app.as_ref(), args.session_key.as_ref()]),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::CreditDeposit { args },
        );
        self.send(&[self.precompile(&digest, &sig), ix], &[]).map(|_| ())
    }

    fn submit_void(&mut self, p: &Pubkey, id: [u8; 32], sig: [u8; 64]) -> Result<()> {
        let digest = void_digest::<Sha256>(self.network, &p.to_bytes(), &id);
        let ix = Self::ledger_ix(
            zol_ledger::accounts::VoidDeposit {
                relayer: self.relayer.pubkey(),
                config: Self::pda(&[seeds::CONFIG]),
                app: Self::pda(&[seeds::APP, p.as_ref()]),
                deposit: Self::pda(&[seeds::DEPOSIT, &id]),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            zol_ledger::instruction::VoidDeposit { deposit_id: id },
        );
        self.send(&[self.precompile(&digest, &sig), ix], &[]).map(|_| ())
    }

    fn submit_paid(&mut self, p: &Pubkey, seq: u64, sig: [u8; 64]) -> Result<()> {
        let digest = paid_digest::<Sha256>(self.network, &p.to_bytes(), seq);
        let app = Self::pda(&[seeds::APP, p.as_ref()]);
        let ix = Self::ledger_ix(
            zol_ledger::accounts::MarkPaid {
                relayer: self.relayer.pubkey(),
                config: Self::pda(&[seeds::CONFIG]),
                app,
                settlement: Self::pda(&[seeds::SETTLEMENT, app.as_ref(), &seq.to_le_bytes()]),
                instructions: IX_SYSVAR,
            },
            zol_ledger::instruction::MarkPaid {},
        );
        self.send(&[self.precompile(&digest, &sig), ix], &[]).map(|_| ())
    }

    fn freeze(&mut self, seed: &[u8; 32], p: &Pubkey) -> Result<()> {
        let g = Keypair::new_from_array(*seed);
        let ix = Self::ledger_ix(
            zol_ledger::accounts::FreezeApp {
                guardian: g.pubkey(),
                config: Self::pda(&[seeds::CONFIG]),
                app: Self::pda(&[seeds::APP, p.as_ref()]),
            },
            zol_ledger::instruction::FreezeApp {},
        );
        self.send(&[ix], &[&g]).map(|_| ())
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    /// A stub RPC that rate-limits the first `limited` requests (alternating
    /// HTTP 429 and a JSON-RPC 429 error), then answers.
    fn stub(limited: usize) -> (String, std::thread::JoinHandle<usize>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let h = std::thread::spawn(move || {
            let mut n = 0;
            for req in server.incoming_requests() {
                n += 1;
                let (code, body) = if n <= limited {
                    if n % 2 == 1 {
                        (429, r#"{"jsonrpc":"2.0","error":{"code":429,"message":"Too many requests"},"id":1}"#)
                    } else {
                        (200, r#"{"jsonrpc":"2.0","error":{"code":429,"message":"Connection rate limits exceeded"},"id":1}"#)
                    }
                } else {
                    (200, r#"{"jsonrpc":"2.0","result":7,"id":1}"#)
                };
                req.respond(tiny_http::Response::from_string(body).with_status_code(code)).unwrap();
                if n > limited {
                    return n;
                }
            }
            n
        });
        (url, h)
    }

    #[test]
    fn rate_limits_are_retried() {
        let (url, h) = stub(2);
        let l = RpcLedger::new(&url, Keypair::new(), [0; 32], 2);
        assert_eq!(l.call("getSlot", json!([])).unwrap(), json!(7));
        assert_eq!(h.join().unwrap(), 3);
    }

    #[test]
    fn persistent_rate_limit_fails_after_five_tries() {
        let (url, _h) = stub(100);
        let l = RpcLedger::new(&url, Keypair::new(), [0; 32], 2);
        let e = l.call("getSlot", json!([])).unwrap_err().to_string();
        assert!(e.contains("429"), "{e}");
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    /// A stub that answers getAccountInfo from the given context slots in turn.
    fn stub(slots: Vec<u64>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        std::thread::spawn(move || {
            for (req, slot) in server.incoming_requests().zip(slots) {
                let body = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":{slot}}},"value":null}}}}"#);
                req.respond(tiny_http::Response::from_string(body)).unwrap();
            }
        });
        url
    }

    #[test]
    fn answers_from_a_lagging_backend_are_refused() {
        let l = RpcLedger::new(&stub(vec![1_000, 1_100, 500, 1_200, 1_000]), Keypair::new(), [0; 32], 2);
        let key = Pubkey::new_unique();
        assert!(l.account_data(&key).is_ok()); // 1,000
        assert!(l.account_data(&key).is_ok()); // 1,100
        let e = l.account_data(&key).unwrap_err().to_string(); // 500: 600 behind
        assert!(e.contains("stale answer"), "{e}");
        assert!(l.account_data(&key).is_ok()); // 1,200
        assert!(l.account_data(&key).is_ok()); // 1,000: 200 behind, within the allowance
    }
}
