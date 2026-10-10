//! Deposit refunds (design v0.3.1, "Deposit"): an Ironwood v6 transaction that spends
//! exactly the deposited note and pays its value minus the ZIP 317 fee to the refund
//! receiver, with **no output addressed to the vault**.
//!
//! Shape: 1 real spend, 2 outputs (refund + one zero-valued padding output added by the
//! coordinator), so 2 actions; the builder adds exactly one dummy *spend*, signed and
//! cleared by IoFinalizer, and never a dummy output (it is configured unpadded). Padding to
//! 2 actions costs nothing (ZIP 317 charges for at least 2 actions) and makes a refund look
//! like an ordinary payment-with-change rather than a 1-action transaction. Every output's
//! OVK follows the policy: the refund uses the vault's external OVK, padding the vault's
//! padding OVK. The refund output carries the receipt memo
//! `zol_wire::receipt::refund_memo(app, deposit cmx)`, the padding output the ZIP 302 empty
//! memo. So every ciphertext is predictable and checked by [`check`]. The padding receiver is a fresh throwaway address.

use std::collections::BTreeMap;

use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use pczt::Pczt;
use rand::RngCore;
use rand_core::CryptoRng;

use zol_wire::receipt::refund_memo;

use crate::{
    checks,
    frost::{self, Identifier, KeyPackage, PublicKeyPackage},
    keys::{AppKeys, Receiver},
    params::ChainParams,
    scan::Spendable,
    tx::{self, OutputSpec, SpendSpec},
    Error, MAX_EXPIRY_DELTA, ZIP317_GRACE_ACTIONS, ZIP317_MARGINAL_FEE,
};

/// A proven, unsigned (vault side) refund PCZT plus what signers need to check it.
#[derive(Clone)]
pub struct RefundPczt {
    pub pczt: Pczt,
    pub app_id: [u8; 32],
    pub deposit_cmx: [u8; 32],
    pub deposit_nullifier: [u8; 32],
    pub deposit_value: u64,
    pub to: Receiver,
    pub refund_value: u64,
    pub fee: u64,
    pub target_height: u32,
    pub expiry_height: u32,
    /// Action index of the real (deposit) spend.
    pub spend_index: usize,
    pub refund_output_index: usize,
    pub padding_output_index: usize,
}

pub(crate) fn throwaway_receiver(rng: &mut impl RngCore) -> Receiver {
    loop {
        let mut b = [0u8; 32];
        rng.fill_bytes(&mut b);
        if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
            return FullViewingKey::from(&sk)
                .address_at(0u32, Scope::External)
                .to_raw_address_bytes();
        }
    }
}

/// Builds, IO-finalizes and proves the refund. `deposit` comes from
/// [`crate::Scanner::spendable`]. `target_height` is the next block's height.
pub fn build(
    params: &ChainParams,
    keys: &AppKeys,
    deposit: &Spendable,
    to: &Receiver,
    target_height: u32,
    expiry_delta: u32,
) -> Result<RefundPczt, Error> {
    if deposit.scanned.app_id != keys.app_id {
        return Err(Error::Build("deposit belongs to another app".into()));
    }
    if keys.receiver_index(to).is_some() {
        return Err(Error::Build("refund receiver is a vault address".into()));
    }
    let fee = ZIP317_MARGINAL_FEE * ZIP317_GRACE_ACTIONS; // 2 actions
    let value = deposit.note.value().inner();
    let refund_value = value
        .checked_sub(fee)
        .filter(|v| *v > 0)
        .ok_or(Error::Build("deposit does not cover the fee".into()))?;
    let outputs = [
        OutputSpec {
            to: *to,
            value: refund_value,
            ovk: Some(keys.ovk(Scope::External)),
            memo: refund_memo(&keys.app_id, &deposit.scanned.cmx),
        },
        OutputSpec {
            to: throwaway_receiver(&mut rand::rngs::OsRng),
            value: 0,
            ovk: Some(keys.padding_ovk()),
            memo: tx::empty_memo(),
        },
    ];
    let spends = [SpendSpec {
        fvk: keys.fvk().clone(),
        note: deposit.note,
        path: deposit.path.clone(),
    }];
    let built = tx::build_ironwood(
        params,
        target_height,
        expiry_delta,
        &spends,
        deposit.anchor,
        &outputs,
        None,
    )?;
    if built.fee != fee {
        return Err(Error::Build(format!("unexpected fee {}", built.fee)));
    }
    let spend_index = built
        .meta
        .spend_action_index(0)
        .ok_or(Error::Build("no spend index".into()))?;
    let refund_output_index = built
        .meta
        .output_action_index(0)
        .ok_or(Error::Build("no output index".into()))?;
    let padding_output_index = built
        .meta
        .output_action_index(1)
        .ok_or(Error::Build("no output index".into()))?;
    let pczt = tx::prove(tx::io_finalize(built.pczt)?)?;
    Ok(RefundPczt {
        pczt,
        app_id: keys.app_id,
        deposit_cmx: deposit.scanned.cmx,
        deposit_nullifier: deposit.scanned.nullifier,
        deposit_value: value,
        to: *to,
        refund_value,
        fee,
        target_height,
        expiry_height: built.expiry_height,
        spend_index,
        refund_output_index,
        padding_output_index,
    })
}

/// What every signer recomputes before signing a refund, from its own copy of the deposit
/// note (`deposit`) and the refund receiver it read from that note's memo (`to`):
/// - no dummy key, no redacted randomness or plaintext, no `user_address`;
/// - exactly one real spend (rk from this app's key and `alpha`) and its nullifier is the
///   deposit's; its `cv_net` matches the note value;
/// - every other spend is a zero-valued, validly signed dummy;
/// - every `cmx` follows from its plaintext; exactly one output carries value: `to`, for
///   value − fee; no output is addressed to the vault (either scope, any diversifier);
/// - the refund output's memo is `refund_memo(app, deposit cmx)`, padding memos are empty;
/// - every `epk`, `enc_ciphertext` and `out_ciphertext` re-encrypts byte-for-byte under the
///   OVK policy (external OVK for the refund, padding OVK for zero-valued outputs);
/// - value balance = fee = ZIP 317 for the action count; expiry within the bound.
///
/// Returns the sighash the signer must sign.
pub fn check(
    r: &RefundPczt,
    keys: &AppKeys,
    deposit: &orchard::Note,
    to: &Receiver,
    current_height: u32,
) -> Result<[u8; 32], Error> {
    check_pczt(&r.pczt, keys, deposit, to, current_height)
}

/// [`check`] on a bare PCZT (as received over the wire for distributed signing).
pub fn check_pczt(
    pczt: &Pczt,
    keys: &AppKeys,
    deposit: &orchard::Note,
    to: &Receiver,
    current_height: u32,
) -> Result<[u8; 32], Error> {
    let r = RefundPcztRef { pczt };
    let sighash = tx::sighash(r.pczt)?;
    let exp = *r.pczt.global().expiry_height();
    if exp <= current_height || exp > current_height + 1 + MAX_EXPIRY_DELTA {
        return Err(Error::Verify(format!(
            "expiry {exp} outside ({current_height}, {}]",
            current_height + 1 + MAX_EXPIRY_DELTA
        )));
    }
    let fvk = keys.fvk();
    let deposit_cmx = orchard::note::ExtractedNoteCommitment::from(deposit.commitment()).to_bytes();
    let receipt = refund_memo(&keys.app_id, &deposit_cmx);
    tx::with_ironwood(r.pczt, |b| -> Result<(), Error> {
        checks::check_complete(b)?;
        let n = b.actions().len() as u64;
        let fee = ZIP317_MARGINAL_FEE * n.max(ZIP317_GRACE_ACTIONS);
        let (mag, sign) = b.value_sum().magnitude_sign();
        if mag != fee || matches!(sign, orchard::value::Sign::Negative) {
            return Err(Error::Verify(format!("value balance {mag} != fee {fee}")));
        }
        let expected_value = deposit
            .value()
            .inner()
            .checked_sub(fee)
            .ok_or(Error::Verify("fee exceeds deposit".into()))?;
        let mut real = 0;
        let mut paid = 0;
        for (i, a) in b.actions().iter().enumerate() {
            if checks::rk_matches(a, fvk) {
                checks::check_real_spend(a, fvk, deposit)
                    .map_err(|e| Error::Verify(format!("action {i}: {e}")))?;
                real += 1;
            } else {
                checks::check_dummy_spend(a, &sighash)
                    .map_err(|e| Error::Verify(format!("action {i}: {e}")))?;
            }
            checks::check_cmx(a)?;
            let o = a.output();
            let recipient = o.recipient().unwrap();
            if fvk.scope_for_address(&recipient).is_some() {
                return Err(Error::Verify(format!(
                    "action {i}: output addressed to the vault"
                )));
            }
            let v = o.value().unwrap().inner();
            let (ovk, memo) = if v > 0 {
                if recipient.to_raw_address_bytes() != *to || v != expected_value {
                    return Err(Error::Verify(format!(
                        "action {i}: unexpected value-carrying output"
                    )));
                }
                paid += 1;
                if checks::recover_memo(a) != Some(receipt) {
                    return Err(Error::Verify(format!(
                        "action {i}: refund memo is not refund_memo(app, deposit cmx)"
                    )));
                }
                (keys.ovk(Scope::External), receipt)
            } else {
                (keys.padding_ovk(), tx::empty_memo())
            };
            checks::check_ciphertexts(a, &memo, &ovk)
                .map_err(|e| Error::Verify(format!("action {i}: {e}")))?;
        }
        if real != 1 || paid != 1 {
            return Err(Error::Verify(format!(
                "{real} real spends, {paid} paid outputs (want 1, 1)"
            )));
        }
        Ok(())
    })??;
    Ok(sighash)
}

struct RefundPcztRef<'a> {
    pczt: &'a Pczt,
}

/// Signs the deposit spend with 2-of-n (or any threshold) FROST under the app's tweaked key.
/// `base_shares` holds the *base* key packages of the participating signers; each signer's
/// checks are simulated by running [`check`] once here. In production every signer runs
/// [`check`] and [`frost::sign_share`] on its own machine.
pub fn sign_with_frost(
    r: RefundPczt,
    keys: &AppKeys,
    deposit: &orchard::Note,
    base_shares: &BTreeMap<Identifier, KeyPackage>,
    base_pubkeys: &PublicKeyPackage,
    current_height: u32,
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<RefundPczt, Error> {
    let sighash = check(&r, keys, deposit, &r.to.clone(), current_height)?;
    let alpha = tx::with_ironwood(&r.pczt, |b| *b.actions()[r.spend_index].spend().alpha())?
        .ok_or(Error::Pczt("alpha".into()))?;
    use ff::PrimeField;
    let mut app_kps = BTreeMap::new();
    let mut app_pk = None;
    for (i, kp) in base_shares {
        let (a, p) = frost::app_packages(keys, kp, base_pubkeys)?;
        app_kps.insert(*i, a);
        app_pk = Some(p);
    }
    let app_pk = app_pk.ok_or(Error::Frost("no signers".into()))?;
    let signers: Vec<_> = app_kps.keys().copied().collect();
    let sig = frost::sign_locally(&app_kps, &app_pk, &signers, &alpha.to_repr(), &sighash, rng)?;
    let pczt = tx::apply_signature(r.pczt, r.spend_index, sig)?;
    Ok(RefundPczt { pczt, ..r })
}

/// Single-key path (tests): `ask_app` is the app's spend-authorizing scalar
/// (`ask_group + t_app`).
pub fn sign_single(r: RefundPczt, ask_app: &[u8; 32]) -> Result<RefundPczt, Error> {
    let pczt = tx::sign_with_scalar(r.pczt, r.spend_index, ask_app)?;
    Ok(RefundPczt { pczt, ..r })
}

/// Extracts and verifies the signed refund. Returns (raw tx, txid internal order).
pub fn extract(r: RefundPczt, params: &ChainParams) -> Result<(Vec<u8>, [u8; 32]), Error> {
    tx::extract(r.pczt, params)
}
