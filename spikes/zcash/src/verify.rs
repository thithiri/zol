//! Independent, offline verification of an extracted transaction.
//!
//! Starting from the raw bytes only: parse, recompute the ZIP 244 shielded sighash from
//! the parsed transaction, then verify (a) every spend-auth signature under its `rk`,
//! (b) the binding signature under the bundle's `bvk`, (c) the Halo2 proof with a
//! freshly built verifying key for the pool's circuit version, and (d) the same via
//! orchard's `BatchValidator`.

use orchard::bundle::BatchValidator;
use zcash_primitives::transaction::{
    sighash::SignableInput,
    sighash_v5::v5_signature_hash,
    sighash_v6::v6_signature_hash,
    TxVersion,
    txid::TxIdDigester,
    Transaction, TransactionData,
};

use crate::pool::Pool;

#[derive(Debug)]
pub struct Report {
    pub txid: String,
    pub tx_version: String,
    pub n_actions: usize,
    pub sighash: [u8; 32],
    pub value_balance: i64,
    pub raw_len: usize,
}

pub fn verify_raw(raw: &[u8], pool: Pool) -> Result<Report, String> {
    let tx = Transaction::read(raw, pool.branch_id()).map_err(|e| format!("parse: {e}"))?;
    // Re-serialization is canonical.
    let mut re = vec![];
    tx.write(&mut re).unwrap();
    if re != raw {
        return Err("re-serialization differs".into());
    }
    let sighash = shielded_sighash(raw, pool)?;

    let bundle = match pool {
        Pool::OrchardNu62 => {
            if tx.ironwood_bundle().is_some() {
                return Err("unexpected ironwood bundle".into());
            }
            tx.orchard_bundle()
        }
        Pool::IronwoodNu63 => {
            if tx.orchard_bundle().is_some() {
                return Err("unexpected orchard bundle".into());
            }
            tx.ironwood_bundle()
        }
    }
    .ok_or("missing shielded bundle")?;

    for (i, a) in bundle.actions().iter().enumerate() {
        a.rk()
            .verify(&sighash, a.authorization())
            .map_err(|e| format!("spend auth sig {i}: {e:?}"))?;
    }
    bundle
        .binding_validating_key()
        .verify(&sighash, bundle.authorization().binding_signature())
        .map_err(|e| format!("binding sig: {e:?}"))?;
    let vk = orchard::circuit::VerifyingKey::build(pool.circuit_version());
    bundle
        .verify_proof(&vk)
        .map_err(|e| format!("proof: {e:?}"))?;
    let mut bv = BatchValidator::new(&vk);
    bv.add_bundle(bundle, sighash).map_err(|e| format!("batch add: {e:?}"))?;
    if !bv.validate(rand::rngs::OsRng) {
        return Err("batch validation failed".into());
    }

    Ok(Report {
        txid: tx.txid().to_string(),
        tx_version: format!("{:?}", tx.version()),
        n_actions: bundle.actions().len(),
        sighash,
        value_balance: i64::from(*bundle.value_balance()),
        raw_len: raw.len(),
    })
}

/// Serializes a transaction.
pub fn raw(tx: &Transaction) -> Vec<u8> {
    let mut v = vec![];
    tx.write(&mut v).unwrap();
    v
}

/// ZIP 244 shielded sighash recomputed from the raw transaction alone. The authorizing
/// data is stripped first (the sighash commits to effects only); transparent inputs are
/// not supported (the vault never has any).
pub fn shielded_sighash(raw: &[u8], pool: Pool) -> Result<[u8; 32], String> {
    let tx = Transaction::read(raw, pool.branch_id()).map_err(|e| format!("parse: {e}"))?;
    let data = tx.into_data();
    if data.transparent_bundle().is_some() || data.sapling_bundle().is_some() {
        return Err("vault payouts must be pure Orchard/Ironwood".into());
    }
    let effects: TransactionData<pczt::EffectsOnly> = data.map_bundles(
        |_| None,
        |_| None,
        |o| o.map(|b| b.map_authorization(&mut (), |_, _, _| (), |_, _| orchard::bundle::EffectsOnly)),
    );
    let txid_parts = effects.digest(TxIdDigester);
    let h = match effects.version() {
        TxVersion::V5 => v5_signature_hash(&effects, &SignableInput::Shielded, &txid_parts),
        TxVersion::V6 => v6_signature_hash(&effects, &SignableInput::Shielded, &txid_parts),
        v => return Err(format!("unsupported tx version {v:?}")),
    };
    Ok(h.as_bytes().try_into().unwrap())
}
