//! Ironwood v6 transactions through the real `pczt` roles (spike 0.3):
//! `zcash_primitives` Builder (drives `orchard::builder::Builder::build_for_pczt`) ->
//! `Creator::build_from_parts` -> IoFinalizer -> Prover -> Signer -> SpendFinalizer ->
//! TxExtractor, plus independent offline verification.

use std::{convert::Infallible, sync::OnceLock};

use orchard::{
    builder::BundleMetadata,
    circuit::{OrchardCircuitVersion, ProvingKey, VerifyingKey},
    keys::{FullViewingKey, OutgoingViewingKey, SpendAuthorizingKey},
    primitives::redpallas::{self, SpendAuth},
    tree::MerklePath,
    Address, Anchor, Note,
};
use pczt::{
    roles::{
        creator::Creator, io_finalizer::IoFinalizer, prover::Prover, signer::Signer,
        spend_finalizer::SpendFinalizer, tx_extractor::TransactionExtractor, verifier::Verifier,
    },
    Pczt,
};
use zcash_primitives::transaction::{
    builder::{BuildConfig, Builder, BundlePadding, PcztResult},
    fees::zip317,
    sighash::SignableInput,
    sighash_v5::v5_signature_hash,
    sighash_v6::v6_signature_hash,
    txid::TxIdDigester,
    Transaction, TransactionData, TxVersion,
};
use zcash_protocol::{
    consensus::BlockHeight, consensus::BranchId, memo::MemoBytes, value::Zatoshis,
};

use crate::{keys::Receiver, params::ChainParams, Error, MAX_EXPIRY_DELTA};

/// The Ironwood (post-NU6.3) circuit's proving key, built once (about 2 s).
pub fn proving_key() -> &'static ProvingKey {
    static PK: OnceLock<ProvingKey> = OnceLock::new();
    PK.get_or_init(|| ProvingKey::build(OrchardCircuitVersion::PostNu6_3))
}

pub fn verifying_key() -> &'static VerifyingKey {
    static VK: OnceLock<VerifyingKey> = OnceLock::new();
    VK.get_or_init(|| VerifyingKey::build(OrchardCircuitVersion::PostNu6_3))
}

/// The ZIP 302 "empty" memo (0xF6 then zeros).
pub fn empty_memo() -> [u8; 512] {
    let mut m = [0u8; 512];
    m[0] = 0xF6;
    m
}

#[derive(Clone, Debug)]
pub struct OutputSpec {
    pub to: Receiver,
    pub value: u64,
    pub ovk: Option<OutgoingViewingKey>,
    pub memo: [u8; 512],
}

#[derive(Clone, Debug)]
pub struct SpendSpec {
    pub fvk: FullViewingKey,
    pub note: Note,
    pub path: MerklePath,
}

/// Change: receives `inputs - outputs - fee`.
#[derive(Clone, Debug)]
pub struct ChangeSpec {
    pub to: Receiver,
    pub ovk: Option<OutgoingViewingKey>,
    pub memo: [u8; 512],
}

pub struct Built {
    pub pczt: Pczt,
    pub meta: BundleMetadata,
    pub fee: u64,
    /// Value of the change output (if a change spec was given).
    pub change_value: u64,
    pub expiry_height: u32,
}

fn addr(r: &Receiver) -> Result<Address, Error> {
    Option::from(Address::from_raw_address_bytes(r))
        .ok_or(Error::Build("receiver does not parse".into()))
}

fn config(anchor: Anchor) -> BuildConfig {
    BuildConfig::Standard {
        sapling_anchor: None,
        orchard_anchor: None,
        ironwood_anchor: Some(anchor),
        orchard_padding: BundlePadding::UNPADDED,
        // The coordinator adds padding itself (design v0.3.1); the builder pads only to the
        // one-action consensus minimum. Dummy *spends* are still added when outputs > spends.
        ironwood_padding: BundlePadding::UNPADDED,
    }
}

/// `target_height + expiry_delta`, capped (as zcashd does) at the block before the next
/// network upgrade after `target_height`: v5/v6 signatures commit to the consensus branch ID,
/// so a transaction left unmined at the upgrade can never be mined, and a lower expiry
/// releases its inputs sooner. Matters for a mainnet NU7 activation.
pub fn capped_expiry<P: zcash_protocol::consensus::Parameters>(
    params: &P,
    target_height: u32,
    expiry_delta: u32,
) -> u32 {
    let e = target_height + expiry_delta;
    match zcash_protocol::consensus::BranchId::for_height(
        params,
        zcash_protocol::consensus::BlockHeight::from_u32(target_height),
    )
    .height_bounds(params)
    .and_then(|(_, next)| next)
    {
        Some(next) => e.min(u32::from(next) - 1).max(target_height),
        None => e,
    }
}

/// Builds an Ironwood v6 PCZT (Creator output). Outputs keep their order: output `i` of
/// `outputs` is `meta.output_action_index(i)`, the change (if any) comes last.
pub fn build_ironwood(
    params: &ChainParams,
    target_height: u32,
    expiry_delta: u32,
    spends: &[SpendSpec],
    anchor: Anchor,
    outputs: &[OutputSpec],
    change: Option<&ChangeSpec>,
) -> Result<Built, Error> {
    if !params.ironwood_active(target_height) {
        return Err(Error::Build("NU6.3 not active at target height".into()));
    }
    if expiry_delta == 0 || expiry_delta > MAX_EXPIRY_DELTA {
        return Err(Error::Build(format!(
            "expiry delta must be in 1..={MAX_EXPIRY_DELTA}"
        )));
    }
    let expiry_height = capped_expiry(params, target_height, expiry_delta);
    let fee_rule = zip317::FeeRule::standard();
    let make = |change_value: u64| -> Result<Builder<ChainParams, ()>, Error> {
        let mut b = Builder::new(
            *params,
            BlockHeight::from_u32(target_height),
            config(anchor),
        )
        .with_expiry_height(BlockHeight::from_u32(expiry_height));
        for s in spends {
            b.add_ironwood_spend::<Infallible>(s.fvk.clone(), s.note, s.path.clone())
                .map_err(|e| Error::Build(format!("spend: {e:?}")))?;
        }
        let ch = change.map(|c| OutputSpec {
            to: c.to,
            value: change_value,
            ovk: c.ovk.clone(),
            memo: c.memo,
        });
        for o in outputs.iter().chain(ch.iter()) {
            b.add_ironwood_output::<Infallible>(
                o.ovk.clone(),
                addr(&o.to)?,
                Zatoshis::from_u64(o.value).map_err(|_| Error::Build("value".into()))?,
                MemoBytes::from_bytes(&o.memo).expect("512 bytes"),
            )
            .map_err(|e| Error::Build(format!("output: {e:?}")))?;
        }
        Ok(b)
    };
    let fee = u64::from(
        make(0)?
            .get_fee(&fee_rule)
            .map_err(|e| Error::Build(format!("fee: {e:?}")))?,
    );
    let inputs: u64 = spends.iter().map(|s| s.note.value().inner()).sum();
    let paid: u64 = outputs.iter().map(|o| o.value).sum();
    let change_value = match change {
        Some(_) => inputs.checked_sub(paid + fee).ok_or(Error::Build(format!(
            "insufficient funds: in {inputs}, out {paid}, fee {fee}"
        )))?,
        None => {
            if inputs != paid + fee {
                return Err(Error::Build(format!(
                    "unbalanced: in {inputs} != out {paid} + fee {fee}"
                )));
            }
            0
        }
    };
    let PcztResult {
        pczt_parts,
        ironwood_meta,
        ..
    } = make(change_value)?
        .build_for_pczt(crate::sys_rng(), &fee_rule)
        .map_err(|e| Error::Build(format!("build_for_pczt: {e:?}")))?;
    let pczt = Creator::build_from_parts(pczt_parts)
        .ok_or(Error::Build("Creator::build_from_parts".into()))?;
    Ok(Built {
        pczt,
        meta: ironwood_meta,
        fee,
        change_value,
        expiry_height,
    })
}

pub fn io_finalize(pczt: Pczt) -> Result<Pczt, Error> {
    IoFinalizer::new(pczt)
        .finalize_io(crate::sys_rng())
        .map_err(|e| Error::Pczt(format!("IoFinalizer: {e:?}")))
}

pub fn prove(pczt: Pczt) -> Result<Pczt, Error> {
    Ok(Prover::new(pczt)
        .create_ironwood_proof(crate::sys_rng(), proving_key())
        .map_err(|e| Error::Pczt(format!("Prover: {e:?}")))?
        .finish())
}

/// The ZIP 244 shielded sighash, computed by the pczt Signer from the effects.
pub fn sighash(pczt: &Pczt) -> Result<[u8; 32], Error> {
    Ok(Signer::new(pczt.clone())
        .map_err(|e| Error::Pczt(format!("Signer: {e:?}")))?
        .shielded_sighash())
}

/// Read-only access to the parsed Ironwood bundle.
pub fn with_ironwood<T>(
    pczt: &Pczt,
    f: impl FnOnce(&orchard::pczt::Bundle) -> T,
) -> Result<T, Error> {
    let mut out = None;
    Verifier::new(pczt.clone())
        .with_ironwood::<Infallible, _>(|b| {
            out = Some(f(b));
            Ok(())
        })
        .map_err(|e| Error::Pczt(format!("parse: {e:?}")))?;
    Ok(out.unwrap())
}

/// Signs action `index` with an ordinary Orchard spending key (wallet path).
pub fn sign_with_ask(pczt: Pczt, index: usize, ask: &SpendAuthorizingKey) -> Result<Pczt, Error> {
    let mut s = Signer::new(pczt).map_err(|e| Error::Pczt(format!("{e:?}")))?;
    s.sign_ironwood(crate::sys_rng(), index, ask)
        .map_err(|e| Error::Pczt(format!("sign: {e:?}")))?;
    Ok(s.finish())
}

/// Signs action `index` with a raw spend-authorizing scalar (single-key vault, tests).
pub fn sign_with_scalar(pczt: Pczt, index: usize, ask: &[u8; 32]) -> Result<Pczt, Error> {
    let sighash = sighash(&pczt)?;
    let alpha = with_ironwood(&pczt, |b| {
        b.actions().get(index).and_then(|a| *a.spend().alpha())
    })?
    .ok_or(Error::Pczt("no alpha for that action".into()))?;
    let sk =
        redpallas::SigningKey::<SpendAuth>::try_from(*ask).map_err(|_| Error::Key("bad ask"))?;
    let sig = sk.randomize(&alpha).sign(crate::sys_rng(), &sighash);
    apply_signature(pczt, index, <[u8; 64]>::from(&sig))
}

/// Applies an externally produced spend-auth signature (verified under `rk` by the Signer).
pub fn apply_signature(pczt: Pczt, index: usize, sig: [u8; 64]) -> Result<Pczt, Error> {
    let mut s = Signer::new(pczt).map_err(|e| Error::Pczt(format!("{e:?}")))?;
    s.apply_ironwood_signature(index, redpallas::Signature::<SpendAuth>::from(sig))
        .map_err(|e| Error::Pczt(format!("apply signature: {e:?}")))?;
    Ok(s.finish())
}

/// SpendFinalizer + TxExtractor (which batch-verifies proof and signatures), then an
/// independent re-verification from the raw bytes. Returns (raw tx, txid internal order).
pub fn extract(pczt: Pczt, params: &ChainParams) -> Result<(Vec<u8>, [u8; 32]), Error> {
    let pczt = SpendFinalizer::new(pczt)
        .finalize_spends()
        .map_err(|e| Error::Pczt(format!("SpendFinalizer: {e:?}")))?;
    let tx = TransactionExtractor::new(pczt)
        .with_orchard(verifying_key())
        .extract(crate::sys_rng())
        .map_err(|e| Error::Pczt(format!("TxExtractor: {e:?}")))?;
    let mut raw = vec![];
    tx.write(&mut raw).map_err(|e| Error::Pczt(e.to_string()))?;
    verify_raw(&raw, params)?;
    Ok((raw, *tx.txid().as_ref()))
}

/// Parses a v5/v6 transaction using the consensus branch ID in its own header.
pub fn parse_tx(raw: &[u8]) -> Result<Transaction, Error> {
    if raw.len() < 12 {
        return Err(Error::Parse("tx too short".into()));
    }
    let branch = BranchId::try_from(u32::from_le_bytes(raw[8..12].try_into().unwrap()))
        .map_err(|_| Error::Parse("unknown branch id (only v5/v6 supported)".into()))?;
    Transaction::read(raw, branch).map_err(|e| Error::Parse(format!("tx: {e}")))
}

/// ZIP 244 shielded sighash recomputed from raw bytes (no transparent or Sapling parts).
pub fn shielded_sighash_raw(raw: &[u8]) -> Result<[u8; 32], Error> {
    let data = parse_tx(raw)?.into_data();
    if data.transparent_bundle().is_some() || data.sapling_bundle().is_some() {
        return Err(Error::Verify(
            "only pure Orchard/Ironwood transactions".into(),
        ));
    }
    let effects: TransactionData<pczt::EffectsOnly> = data.map_bundles(
        |_| None,
        |_| None,
        |o| {
            o.map(|b| {
                b.map_authorization(&mut (), |_, _, _| (), |_, _| orchard::bundle::EffectsOnly)
            })
        },
    );
    let parts = effects.digest(TxIdDigester);
    let h = match effects.version() {
        TxVersion::V5 => v5_signature_hash(&effects, &SignableInput::Shielded, &parts),
        TxVersion::V6 => v6_signature_hash(&effects, &SignableInput::Shielded, &parts),
        v => return Err(Error::Verify(format!("unsupported version {v:?}"))),
    };
    Ok(h.as_bytes().try_into().unwrap())
}

/// Offline verification of a ZOL-built transaction: canonical encoding, ZIP 244 sighash
/// recomputed from the bytes, every spend-auth signature, the binding signature and the
/// Halo2 proof of the Ironwood bundle (PostNu6_3 verifying key). No Orchard-pool bundle.
pub fn verify_raw(raw: &[u8], _params: &ChainParams) -> Result<[u8; 32], Error> {
    let tx = parse_tx(raw)?;
    let mut re = vec![];
    tx.write(&mut re)
        .map_err(|e| Error::Verify(e.to_string()))?;
    if re != raw {
        return Err(Error::Verify("non-canonical encoding".into()));
    }
    if tx.orchard_bundle().is_some() {
        return Err(Error::Verify("unexpected Orchard-pool bundle".into()));
    }
    let bundle = tx
        .ironwood_bundle()
        .ok_or(Error::Verify("no Ironwood bundle".into()))?;
    let sighash = shielded_sighash_raw(raw)?;
    for (i, a) in bundle.actions().iter().enumerate() {
        a.rk()
            .verify(&sighash, a.authorization())
            .map_err(|e| Error::Verify(format!("spend auth sig {i}: {e:?}")))?;
    }
    bundle
        .binding_validating_key()
        .verify(&sighash, bundle.authorization().binding_signature())
        .map_err(|e| Error::Verify(format!("binding sig: {e:?}")))?;
    bundle
        .verify_proof(verifying_key())
        .map_err(|e| Error::Verify(format!("proof: {e:?}")))?;
    Ok(*tx.txid().as_ref())
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    use crate::params::{Network, RegtestHeights};

    #[test]
    fn expiry_stops_before_the_next_upgrade() {
        let p = ChainParams {
            network: Network::Regtest,
            regtest: RegtestHeights {
                nu7: Some(50),
                ..RegtestHeights::PRE_NU7
            },
        };
        assert_eq!(capped_expiry(&p, 5, 40), 45);
        assert_eq!(capped_expiry(&p, 45, 40), 49);
        assert_eq!(capped_expiry(&p, 49, 40), 49);
        assert_eq!(capped_expiry(&p, 50, 40), 90);
        assert_eq!(
            capped_expiry(&ChainParams::TESTNET, 4_465_000, 40),
            4_465_025
        );
        assert_eq!(
            capped_expiry(&ChainParams::MAINNET, 3_600_000, 40),
            3_600_040
        );
    }
}
