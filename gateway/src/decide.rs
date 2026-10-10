//! Deposit decisions (plan 3.4; design "Deposit" state machine).
//!
//! Each operator decides independently from its own scan. The decision is pure:
//! the same note, memo and ledger state give the same result on every operator,
//! which is what lets a threshold of them sign the same statement.

use zol_wire::memo::{classify, MemoKind};
use zol_wire::{CreditAttestation, DepositMemo, Network, MEMO_LEN, ORCHARD_ADDR_LEN};

use crate::chain::{AddressCheck, AppId, ScannedNote};
use crate::config::AppSecrets;

/// Ledger-side facts a decision depends on.
#[derive(Clone, Copy, Debug)]
pub struct AppStatus {
    /// Active, not frozen, and the ledger is not paused.
    pub accepting: bool,
    pub entry_fee_bps: u16,
    /// deposit_cap − liability − unpaid_total, saturating.
    pub cap_room: u64,
}

pub struct DecideCtx<'a> {
    pub app: AppId,
    pub network: Network,
    /// The address the session key signs: the app's primary vault address.
    pub vault_address: [u8; ORCHARD_ADDR_LEN],
    pub tip: u32,
    pub denominations: &'a [u64],
    pub secrets: &'a AppSecrets,
    pub addresses: &'a dyn AddressCheck,
    pub status: AppStatus,
    /// Memo registered off-chain for the receiver this note was sent to, if any.
    pub intent: Option<[u8; MEMO_LEN]>,
    /// Payout commitment already on the session's ledger balance, if any.
    pub existing_commitment: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoSource {
    Chain,
    Intent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoidReason {
    BadSignature,
    Expired,
    BadDenomination,
    AppUnavailable,
    OverCap,
    PayoutAddressChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldReason {
    /// No ZOL memo on chain and no registered intent for the receiver.
    NoMemo,
    /// A memo, but its payout address does not parse: nowhere to refund.
    NoRefundAddress,
}

/// Credit attestation fields, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credit {
    pub deposit_id: [u8; 32],
    pub amount: u64,
    pub session_key: [u8; 32],
    pub payout_commitment: [u8; 32],
    pub flags: u8,
    pub pot_id: [u8; 32],
    pub app_data: Vec<u8>,
}

impl Credit {
    pub fn attestation(&self, network: Network, app: &AppId) -> CreditAttestation<'_> {
        CreditAttestation {
            network: network as u8,
            app: *app,
            deposit_id: self.deposit_id,
            amount: self.amount,
            session_key: self.session_key,
            payout_commitment: self.payout_commitment,
            flags: self.flags,
            pot_id: self.pot_id,
            app_data: &self.app_data,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Credit { credit: Credit, memo: [u8; MEMO_LEN], source: MemoSource },
    Void { deposit_id: [u8; 32], reason: VoidReason, memo: [u8; MEMO_LEN], refund_to: [u8; ORCHARD_ADDR_LEN] },
    Hold { reason: HoldReason },
    /// A head or top-up note: vault bookkeeping, not a deposit.
    NotDeposit(MemoKind),
}

/// The canonical memo a note carries, or the registered intent for its receiver.
pub fn resolve_memo(note: &ScannedNote, intent: Option<&[u8; MEMO_LEN]>) -> Option<(DepositMemo, [u8; MEMO_LEN], MemoSource)> {
    if let Ok(m) = DepositMemo::decode_any(&crate::chain::memo_array(note)) {
        let canon = m.encode();
        return Some((m, canon, MemoSource::Chain));
    }
    let intent = intent?;
    let m = DepositMemo::decode_any(intent).ok()?;
    let canon = m.encode();
    Some((m, canon, MemoSource::Intent))
}

pub fn decide(note: &ScannedNote, ctx: &DecideCtx) -> Decision {
    if let Ok(kind @ (MemoKind::HeadV1 | MemoKind::TopUpV1)) = classify(&crate::chain::memo_array(note)) {
        return Decision::NotDeposit(kind);
    }
    let deposit_id = ctx.secrets.deposit_id(&note.cmx);
    let Some((memo, canon, source)) = resolve_memo(note, ctx.intent.as_ref()) else {
        return Decision::Hold { reason: HoldReason::NoMemo };
    };
    if !ctx.addresses.is_valid_receiver(&memo.payout_address) {
        return Decision::Hold { reason: HoldReason::NoRefundAddress };
    }
    let void = |reason| Decision::Void { deposit_id, reason, memo: canon, refund_to: memo.payout_address };
    if memo.verify_signature(ctx.network, &ctx.vault_address).is_err() {
        return void(VoidReason::BadSignature);
    }
    if ctx.tip as u64 >= memo.expiry_height as u64 {
        return void(VoidReason::Expired);
    }
    if !ctx.denominations.contains(&note.value) {
        return void(VoidReason::BadDenomination);
    }
    if !ctx.status.accepting {
        return void(VoidReason::AppUnavailable);
    }
    let net = note.value - zol_ledger::state::bps(note.value, ctx.status.entry_fee_bps);
    if net > ctx.status.cap_room {
        return void(VoidReason::OverCap);
    }
    let payout_commitment = ctx.secrets.payout_commitment(&memo.payout_address, &memo.session_key, &ctx.app);
    if ctx.existing_commitment.is_some_and(|c| c != payout_commitment) {
        return void(VoidReason::PayoutAddressChanged);
    }
    Decision::Credit {
        credit: Credit {
            deposit_id,
            amount: note.value,
            session_key: memo.session_key,
            payout_commitment,
            flags: memo.flags,
            pot_id: memo.pot_id,
            app_data: memo.app_data().to_vec(),
        },
        memo: canon,
        source,
    }
}

/// Recomputes the credit digest from a note and the memo it was decided from
/// (design signer rule 4: "the stored attestation digest equals the digest the
/// signer recomputes from that note and its memo").
pub fn recompute_credit_digest(
    note: &ScannedNote,
    memo: &[u8; MEMO_LEN],
    network: Network,
    vault_address: &[u8; ORCHARD_ADDR_LEN],
    secrets: &AppSecrets,
) -> Option<[u8; 32]> {
    let m = DepositMemo::decode(memo).ok()?;
    m.verify_signature(network, vault_address).ok()?;
    let credit = Credit {
        deposit_id: secrets.deposit_id(&note.cmx),
        amount: note.value,
        session_key: m.session_key,
        payout_commitment: secrets.payout_commitment(&m.payout_address, &m.session_key, &note.app),
        flags: m.flags,
        pot_id: m.pot_id,
        app_data: m.app_data().to_vec(),
    };
    Some(credit.attestation(network, &note.app).digest::<zol_wire::digest::Sha256>())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chain::{PermissiveAddresses, Scope};
    use ed25519_dalek::{Signer, SigningKey};

    pub const APP: AppId = [0x44; 32];
    pub const VAULT: [u8; 43] = [0x33; 43];

    pub fn signed_memo(sk: &SigningKey, expiry: u32, addr: [u8; 43]) -> DepositMemo {
        let mut m = DepositMemo::new(0, sk.verifying_key().to_bytes(), addr, expiry, [0; 32], b"seat").unwrap();
        let sig = sk.sign(&m.signing_message(Network::Testnet, &VAULT));
        m.set_signature(sig.to_bytes());
        m
    }

    pub fn note_with(memo: &[u8; 512], value: u64) -> ScannedNote {
        ScannedNote {
            app: APP,
            cmx: [9; 32],
            nullifier: [8; 32],
            value,
            memo: memo.to_vec(),
            receiver: VAULT.to_vec(),
            receiver_index: 0,
            scope: Scope::External,
            height: 100,
            block_hash: [1; 32],
            txid: [2; 32],
        }
    }

    pub fn ctx<'a>(secrets: &'a AppSecrets, denoms: &'a [u64]) -> DecideCtx<'a> {
        DecideCtx {
            app: APP,
            network: Network::Testnet,
            vault_address: VAULT,
            tip: 200,
            denominations: denoms,
            secrets,
            addresses: &PermissiveAddresses,
            status: AppStatus { accepting: true, entry_fee_bps: 30, cap_room: u64::MAX },
            intent: None,
            existing_commitment: None,
        }
    }

    fn secrets() -> AppSecrets {
        AppSecrets::derive(&[5; 32], Network::Testnet, &APP)
    }

    #[test]
    fn credit_from_binary_and_text_memo_is_identical() {
        let s = secrets();
        let d = [1_000_000];
        let sk = SigningKey::from_bytes(&[3; 32]);
        let m = signed_memo(&sk, 1_000, [7; 43]);
        let a = decide(&note_with(&m.encode(), 1_000_000), &ctx(&s, &d));
        let b = decide(&note_with(&m.encode_text().unwrap().0, 1_000_000), &ctx(&s, &d));
        assert!(matches!(a, Decision::Credit { .. }));
        assert_eq!(a, b);
        if let Decision::Credit { credit, memo, .. } = a {
            let n = note_with(&memo, 1_000_000);
            assert_eq!(
                recompute_credit_digest(&n, &memo, Network::Testnet, &VAULT, &s),
                Some(credit.attestation(Network::Testnet, &APP).digest::<zol_wire::digest::Sha256>())
            );
        }
    }

    #[test]
    fn void_reasons() {
        let s = secrets();
        let d = [1_000_000];
        let sk = SigningKey::from_bytes(&[3; 32]);
        let good = signed_memo(&sk, 1_000, [7; 43]);
        let reason = |memo: &[u8; 512], value, f: &dyn Fn(&mut DecideCtx)| {
            let mut c = ctx(&s, &d);
            f(&mut c);
            match decide(&note_with(memo, value), &c) {
                Decision::Void { reason, refund_to, .. } => {
                    assert_eq!(refund_to, [7; 43]);
                    Some(reason)
                }
                _ => None,
            }
        };
        let mut forged = good.clone();
        forged.expiry_height = 2_000; // breaks the signature
        assert_eq!(reason(&forged.encode(), 1_000_000, &|_| {}), Some(VoidReason::BadSignature));
        assert_eq!(reason(&signed_memo(&sk, 200, [7; 43]).encode(), 1_000_000, &|_| {}), Some(VoidReason::Expired));
        assert_eq!(reason(&good.encode(), 999_999, &|_| {}), Some(VoidReason::BadDenomination));
        assert_eq!(reason(&good.encode(), 1_000_000, &|c| c.status.accepting = false), Some(VoidReason::AppUnavailable));
        assert_eq!(reason(&good.encode(), 1_000_000, &|c| c.status.cap_room = 996_999), Some(VoidReason::OverCap));
        assert_eq!(reason(&good.encode(), 1_000_000, &|c| c.status.cap_room = 997_000), None);
        assert_eq!(
            reason(&good.encode(), 1_000_000, &|c| c.existing_commitment = Some([1; 32])),
            Some(VoidReason::PayoutAddressChanged)
        );
    }

    #[test]
    fn holds_and_intents() {
        let s = secrets();
        let d = [1_000_000];
        let sk = SigningKey::from_bytes(&[3; 32]);
        // Memo dropped by the wallet.
        let empty = {
            let mut m = [0u8; 512];
            m[0] = 0xF6;
            m
        };
        assert_eq!(decide(&note_with(&empty, 1_000_000), &ctx(&s, &d)), Decision::Hold { reason: HoldReason::NoMemo });
        // The page registered the memo for this receiver.
        let m = signed_memo(&sk, 1_000, [7; 43]);
        let mut c = ctx(&s, &d);
        c.intent = Some(m.encode());
        assert!(matches!(decide(&note_with(&empty, 1_000_000), &c), Decision::Credit { source: MemoSource::Intent, .. }));
        // Unparseable refund address.
        let bad = signed_memo(&sk, 1_000, [0xFF; 43]);
        assert_eq!(decide(&note_with(&bad.encode(), 1_000_000), &ctx(&s, &d)), Decision::Hold { reason: HoldReason::NoRefundAddress });
        // Head and top-up notes are not deposits.
        let head = zol_wire::HeadMemo::genesis(Network::Testnet, APP).encode();
        assert_eq!(decide(&note_with(&head, 5), &ctx(&s, &d)), Decision::NotDeposit(MemoKind::HeadV1));
    }
}
