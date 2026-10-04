//! Spike 0.3: build a one-input vault payout as a PCZT.
//!
//! Path: `zcash_primitives::transaction::builder::Builder` (which drives
//! `orchard::builder::Builder::build_for_pczt`) -> `PcztParts` -> pczt `Creator::build_from_parts`.
//! The later roles (IoFinalizer, Prover, Signer, SpendFinalizer, TxExtractor) are run by
//! the caller so each spike can stop at the stage it inspects.

use std::convert::Infallible;

use orchard::{
    builder::BundleMetadata,
    keys::{FullViewingKey, OutgoingViewingKey},
    tree::MerklePath,
    Address, Anchor, Note,
};
use pczt::{
    roles::{
        creator::Creator, io_finalizer::IoFinalizer, prover::Prover, signer::Signer,
        spend_finalizer::SpendFinalizer, tx_extractor::TransactionExtractor,
    },
    Pczt,
};
use zcash_primitives::transaction::{
    builder::{BuildConfig, Builder, BundlePadding, PcztResult},
    fees::zip317,
    Transaction,
};
use zcash_protocol::{consensus::TEST_NETWORK, memo::MemoBytes, value::Zatoshis};

use crate::pool::Pool;

pub struct Output {
    pub to: Address,
    pub value: u64,
    pub ovk: Option<OutgoingViewingKey>,
    pub memo: [u8; 512],
}

pub struct PayoutRequest {
    pub pool: Pool,
    pub vault_fvk: FullViewingKey,
    pub spends: Vec<(Note, MerklePath)>,
    pub anchor: Anchor,
    /// External payouts.
    pub payouts: Vec<Output>,
    /// The new head (change). Its value is computed: inputs - payouts - fee.
    pub head_to: Address,
    pub head_ovk: Option<OutgoingViewingKey>,
    pub head_memo: [u8; 512],
    /// Explicit zero-valued padding outputs (to throwaway addresses).
    pub padding: Vec<Output>,
}

pub struct Built {
    pub pczt: Pczt,
    pub meta: BundleMetadata,
    pub fee: u64,
    pub head_value: u64,
}

fn config(pool: Pool, anchor: Anchor) -> BuildConfig {
    BuildConfig::Standard {
        sapling_anchor: None,
        orchard_anchor: (pool == Pool::OrchardNu62).then_some(anchor),
        ironwood_anchor: (pool == Pool::IronwoodNu63).then_some(anchor),
        orchard_padding: BundlePadding::DEFAULT,
        ironwood_padding: BundlePadding::DEFAULT,
    }
}

fn memo(m: &[u8; 512]) -> MemoBytes {
    MemoBytes::from_bytes(&m[..]).expect("512 bytes")
}

fn add_all(
    b: &mut Builder<zcash_protocol::consensus::TestNetwork, ()>,
    req: &PayoutRequest,
    head_value: u64,
) {
    for (note, path) in &req.spends {
        match req.pool {
            Pool::OrchardNu62 => b.add_orchard_spend::<Infallible>(req.vault_fvk.clone(), *note, path.clone()),
            Pool::IronwoodNu63 => b.add_ironwood_spend::<Infallible>(req.vault_fvk.clone(), *note, path.clone()),
        }
        .expect("add spend");
    }
    let outs = req
        .payouts
        .iter()
        .map(|o| (o.ovk.clone(), o.to, o.value, o.memo))
        .chain(std::iter::once((req.head_ovk.clone(), req.head_to, head_value, req.head_memo)))
        .chain(req.padding.iter().map(|o| (o.ovk.clone(), o.to, o.value, o.memo)));
    for (ovk, to, value, m) in outs {
        let v = Zatoshis::from_u64(value).unwrap();
        match req.pool {
            Pool::OrchardNu62 => b.add_orchard_output::<Infallible>(ovk, to, v, memo(&m)),
            Pool::IronwoodNu63 => b.add_ironwood_output::<Infallible>(ovk, to, v, memo(&m)),
        }
        .expect("add output");
    }
}

/// Builds the payout PCZT (Creator output; IO not yet finalized).
pub fn build(req: &PayoutRequest) -> Built {
    let fee_rule = zip317::FeeRule::standard();
    // Fee depends only on the action count, so size it with a placeholder head value.
    let fee = {
        let mut b = Builder::new(TEST_NETWORK, req.pool.target_height(), config(req.pool, req.anchor));
        add_all(&mut b, req, 0);
        u64::from(b.get_fee(&fee_rule).expect("fee"))
    };
    let inputs: u64 = req.spends.iter().map(|(n, _)| n.value().inner()).sum();
    let paid: u64 = req.payouts.iter().map(|o| o.value).sum::<u64>()
        + req.padding.iter().map(|o| o.value).sum::<u64>();
    let head_value = inputs - paid - fee;

    let mut b = Builder::new(TEST_NETWORK, req.pool.target_height(), config(req.pool, req.anchor));
    add_all(&mut b, req, head_value);
    let PcztResult {
        pczt_parts,
        orchard_meta,
        ironwood_meta,
        ..
    } = b
        .build_for_pczt(rand::rngs::OsRng, &fee_rule)
        .expect("build_for_pczt");
    let pczt = Creator::build_from_parts(pczt_parts).expect("Creator::build_from_parts");
    Built {
        pczt,
        meta: match req.pool {
            Pool::OrchardNu62 => orchard_meta,
            Pool::IronwoodNu63 => ironwood_meta,
        },
        fee,
        head_value,
    }
}

pub fn io_finalize(pczt: Pczt) -> Pczt {
    IoFinalizer::new(pczt).finalize_io().expect("IoFinalizer")
}

pub fn prove(pczt: Pczt, pool: Pool) -> Pczt {
    let p = Prover::new(pczt);
    match pool {
        Pool::OrchardNu62 => p.create_orchard_proof(pool.proving_key()).expect("orchard proof"),
        Pool::IronwoodNu63 => p.create_ironwood_proof(pool.proving_key()).expect("ironwood proof"),
    }
    .finish()
}

pub fn shielded_sighash(pczt: &Pczt) -> [u8; 32] {
    Signer::new(pczt.clone()).expect("Signer::new").shielded_sighash()
}

/// Signs action `index` with a plain (single-key) spend authorizing key via the pczt Signer.
pub fn sign_single(pczt: Pczt, pool: Pool, index: usize, ask: &orchard::keys::SpendAuthorizingKey) -> Pczt {
    let mut s = Signer::new(pczt).expect("Signer::new");
    match pool {
        Pool::OrchardNu62 => s.sign_orchard(index, ask),
        Pool::IronwoodNu63 => s.sign_ironwood(index, ask),
    }
    .expect("sign");
    s.finish()
}

/// Applies an externally produced spend-auth signature (e.g. FROST aggregate).
pub fn apply_signature(pczt: Pczt, pool: Pool, index: usize, sig: [u8; 64]) -> Result<Pczt, String> {
    let mut s = Signer::new(pczt).map_err(|e| format!("{e:?}"))?;
    let sig = orchard::primitives::redpallas::Signature::<orchard::primitives::redpallas::SpendAuth>::from(sig);
    match pool {
        Pool::OrchardNu62 => s.apply_orchard_signature(index, sig),
        Pool::IronwoodNu63 => s.apply_ironwood_signature(index, sig),
    }
    .map_err(|e| format!("{e:?}"))?;
    Ok(s.finish())
}

pub fn finalize_spends(pczt: Pczt) -> Pczt {
    SpendFinalizer::new(pczt).finalize_spends().expect("SpendFinalizer")
}

pub fn extract(pczt: Pczt, pool: Pool) -> Result<Transaction, String> {
    TransactionExtractor::new(pczt)
        .with_orchard(pool.verifying_key())
        .extract()
        .map_err(|e| format!("{e:?}"))
}
