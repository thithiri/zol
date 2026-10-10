//! Outputs of a transaction addressed to a vault, and offline transaction verification.
//!
//! "Addressed to the vault" (design v0.3.1, "The vault head") means: decrypts under the
//! vault's incoming viewing key in either scope, for any diversified address. Trial
//! decryption with an `ivk` already covers every diversifier.

use orchard::{
    keys::Scope,
    note_encryption::{IronwoodDomain, OrchardDomain},
    Note,
};
use zcash_note_encryption::try_note_decryption;

use crate::{keys::AppKeys, scan::Pool, tx, Error};

pub use crate::tx::{shielded_sighash_raw, verify_raw};

#[derive(Clone, Debug)]
pub struct VaultOutput {
    pub pool: Pool,
    pub action_index: u32,
    pub scope: Scope,
    pub receiver: [u8; 43],
    pub receiver_index: [u8; 11],
    pub value: u64,
    pub memo: [u8; 512],
    pub cmx: [u8; 32],
    pub note: Note,
}

/// Every output of `raw_tx` (Ironwood and Orchard bundles) that decrypts under `keys`.
pub fn vault_outputs(raw_tx: &[u8], keys: &AppKeys) -> Result<Vec<VaultOutput>, Error> {
    let tx = tx::parse_tx(raw_tx)?;
    let ivks = [Scope::External, Scope::Internal].map(|s| (s, keys.ivk(s)));
    let mut out = vec![];
    for (pool, bundle) in [
        (Pool::Ironwood, tx.ironwood_bundle()),
        (Pool::Orchard, tx.orchard_bundle()),
    ] {
        let Some(bundle) = bundle else { continue };
        for (i, a) in bundle.actions().iter().enumerate() {
            for (scope, ivk) in &ivks {
                let pivk = ivk.prepare();
                let r = match pool {
                    Pool::Ironwood => try_note_decryption(&IronwoodDomain::for_action(a), &pivk, a),
                    Pool::Orchard => try_note_decryption(&OrchardDomain::for_action(a), &pivk, a),
                };
                if let Some((note, addr, memo)) = r {
                    out.push(VaultOutput {
                        pool,
                        action_index: i as u32,
                        scope: *scope,
                        receiver: addr.to_raw_address_bytes(),
                        receiver_index: ivk
                            .diversifier_index(&addr)
                            .map(|d| *d.as_bytes())
                            .unwrap_or([0; 11]),
                        value: note.value().inner(),
                        memo,
                        cmx: a.cmx().to_bytes(),
                        note,
                    });
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Nullifiers revealed by `raw_tx` (Ironwood then Orchard), for walking the head chain.
pub fn nullifiers(raw_tx: &[u8]) -> Result<Vec<(Pool, [u8; 32])>, Error> {
    let tx = tx::parse_tx(raw_tx)?;
    let mut v = vec![];
    for (pool, bundle) in [
        (Pool::Ironwood, tx.ironwood_bundle()),
        (Pool::Orchard, tx.orchard_bundle()),
    ] {
        if let Some(b) = bundle {
            v.extend(b.actions().iter().map(|a| (pool, a.nullifier().to_bytes())));
        }
    }
    Ok(v)
}
