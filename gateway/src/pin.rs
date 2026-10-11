//! Design rule 2: signers pin the ledger's code. Every operator is configured with the
//! SHA-256 and length of the ledger program it audited (`target/deploy/zol_ledger.so` from
//! `scripts/build-sbf.sh`, as in the README) and checks the deployed program against it. On a
//! mismatch it signs nothing: no attestation round, no payout round, no answer to a peer's
//! signing request. Reads and the page API keep working.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anchor_lang::prelude::Pubkey;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The upgradeable loader; a program's code lives in its ProgramData account.
pub const BPF_UPGRADEABLE_LOADER: Pubkey = anchor_lang::prelude::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
/// ProgramData header: enum tag (4), slot (8), option tag (1), authority (32).
pub const PROGRAM_DATA_HEADER: usize = 45;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LedgerCode {
    /// Hex SHA-256 of the program's `.so`.
    pub sha256: String,
    /// Its length in bytes: the ProgramData account is longer (extensions are zero padding).
    pub len: usize,
}

pub fn program_data_address(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[program.as_ref()], &BPF_UPGRADEABLE_LOADER).0
}

/// Whether a ProgramData account's bytes hold exactly the pinned code (then only padding).
pub fn matches(program_data: &[u8], pin: &LedgerCode) -> bool {
    let Some(code) = program_data.get(PROGRAM_DATA_HEADER..PROGRAM_DATA_HEADER + pin.len) else { return false };
    let padding_is_zero = program_data[PROGRAM_DATA_HEADER + pin.len..].iter().all(|b| *b == 0);
    hex::encode(Sha256::digest(code)) == pin.sha256.to_lowercase() && padding_is_zero
}

/// The operator's view of the pin, shared by the loop and the peer API. Without a pin
/// configured (local development) it is always open, and says so once at startup.
#[derive(Clone)]
pub struct Pin {
    pub code: Option<LedgerCode>,
    ok: Arc<AtomicBool>,
    checked_at: Arc<std::sync::atomic::AtomicI64>,
}

impl Pin {
    pub fn new(code: Option<LedgerCode>) -> Self {
        let open = code.is_none();
        Pin { code, ok: Arc::new(AtomicBool::new(open)), checked_at: Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN)) }
    }

    pub fn ok(&self) -> bool {
        self.ok.load(Ordering::SeqCst)
    }

    /// Re-reads the program if `every_secs` have passed. `read` returns the ProgramData bytes.
    /// A failed read keeps the last answer (a flaky RPC must not stop the operator, nor reopen
    /// it). Returns `Some(now_ok)` when the answer changed.
    pub fn refresh(&self, now: i64, every_secs: i64, read: impl FnOnce() -> Option<Vec<u8>>) -> Option<bool> {
        let Some(code) = &self.code else { return None };
        if now.saturating_sub(self.checked_at.load(Ordering::SeqCst)) < every_secs {
            return None;
        }
        let data = read()?;
        self.checked_at.store(now, Ordering::SeqCst);
        let ok = matches(&data, code);
        let was = self.ok.swap(ok, Ordering::SeqCst);
        (was != ok || now == self.checked_at.load(Ordering::SeqCst) && !ok).then_some(ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pd(code: &[u8], pad: usize) -> Vec<u8> {
        let mut d = vec![3u8; PROGRAM_DATA_HEADER];
        d.extend_from_slice(code);
        d.extend(std::iter::repeat_n(0, pad));
        d
    }

    #[test]
    fn matches_the_pinned_code_with_padding_only() {
        let code = b"\x7fELF fake ledger";
        let pin = LedgerCode { sha256: hex::encode(Sha256::digest(code)), len: code.len() };
        assert!(matches(&pd(code, 0), &pin));
        assert!(matches(&pd(code, 20_480), &pin), "extended program data");
        let mut other = pd(code, 10);
        other[PROGRAM_DATA_HEADER + 3] ^= 1;
        assert!(!matches(&other, &pin), "one changed byte");
        let mut tail = pd(code, 10);
        *tail.last_mut().unwrap() = 1;
        assert!(!matches(&tail, &pin), "code beyond the pinned length");
        assert!(!matches(&pd(&code[..5], 0), &pin), "shorter than the pin");
    }

    #[test]
    fn refresh_closes_on_mismatch_keeps_the_answer_on_read_errors() {
        let code = b"ledger v1";
        let pin = Pin::new(Some(LedgerCode { sha256: hex::encode(Sha256::digest(code)), len: code.len() }));
        assert!(!pin.ok(), "closed until checked");
        assert_eq!(pin.refresh(100, 60, || Some(pd(code, 0))), Some(true));
        assert!(pin.ok());
        assert_eq!(pin.refresh(120, 60, || panic!("not due")), None);
        assert_eq!(pin.refresh(200, 60, || None), None, "read failed: keep");
        assert!(pin.ok());
        assert_eq!(pin.refresh(300, 60, || Some(pd(b"ledger v2", 0))), Some(false));
        assert!(!pin.ok());
        assert!(Pin::new(None).ok(), "no pin configured: open (local development)");
    }
}
