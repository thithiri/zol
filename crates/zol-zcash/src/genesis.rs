//! The genesis head (design v0.3.1, "The vault head"): the gateway pays the app's float from
//! a funding note into the vault's internal address with `HeadMemo::genesis(network, app)`.
//! `activate_app` stores the returned `head_cmx`.
//!
//! The head is the transaction's only vault-addressed output; change goes back to the
//! funding wallet's internal address. OVKs: the head uses the vault's internal OVK (the
//! head policy), the change the funding wallet's internal OVK.

use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey};
use zol_wire::HeadMemo;

use crate::{
    keys::AppKeys,
    params::ChainParams,
    scan::Spendable,
    tx::{self, ChangeSpec, OutputSpec, SpendSpec},
    verify, Error,
};

#[derive(Clone, Debug)]
pub struct GenesisTx {
    pub raw_tx: Vec<u8>,
    /// Internal byte order.
    pub txid: [u8; 32],
    /// The head note's commitment (`cmx`), as `activate_app` records it.
    pub head_cmx: [u8; 32],
    pub head_value: u64,
    pub fee: u64,
    pub change_value: u64,
}

/// Builds, proves, signs (single funding key) and extracts the genesis head transaction.
pub fn build(
    params: &ChainParams,
    funding_sk: &SpendingKey,
    funding: &Spendable,
    app: &AppKeys,
    float: u64,
    target_height: u32,
    expiry_delta: u32,
) -> Result<GenesisTx, Error> {
    build_multi(
        params,
        funding_sk,
        std::slice::from_ref(funding),
        app,
        float,
        target_height,
        expiry_delta,
    )
}

/// Like [`build`], funded from several notes of the same key. All `funding` entries must
/// share one anchor (take them from `Scanner::spendable` with the same anchor height).
pub fn build_multi(
    params: &ChainParams,
    funding_sk: &SpendingKey,
    funding: &[Spendable],
    app: &AppKeys,
    float: u64,
    target_height: u32,
    expiry_delta: u32,
) -> Result<GenesisTx, Error> {
    let ffvk = FullViewingKey::from(funding_sk);
    let first = funding
        .first()
        .ok_or(Error::Build("no funding notes".into()))?;
    for f in funding {
        if ffvk.scope_for_address(&f.note.recipient()).is_none() {
            return Err(Error::Build(
                "funding key does not own a funding note".into(),
            ));
        }
        if f.anchor != first.anchor {
            return Err(Error::Build("funding notes have different anchors".into()));
        }
    }
    if float == 0 {
        return Err(Error::Build("float must be positive".into()));
    }
    let memo = HeadMemo::genesis(app.network, app.app_id).encode();
    let outputs = [OutputSpec {
        to: app.vault_internal_address(),
        value: float,
        ovk: Some(app.ovk(Scope::Internal)),
        memo,
    }];
    let change = ChangeSpec {
        to: ffvk
            .address_at(0u32, Scope::Internal)
            .to_raw_address_bytes(),
        ovk: Some(ffvk.to_ovk(Scope::Internal)),
        memo: tx::empty_memo(),
    };
    let spends: Vec<SpendSpec> = funding
        .iter()
        .map(|f| SpendSpec {
            fvk: ffvk.clone(),
            note: f.note,
            path: f.path.clone(),
        })
        .collect();
    let built = tx::build_ironwood(
        params,
        target_height,
        expiry_delta,
        &spends,
        first.anchor,
        &outputs,
        Some(&change),
    )?;
    let spend_indices = (0..spends.len())
        .map(|i| built.meta.spend_action_index(i))
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::Build("spend index".into()))?;
    let head_index = built
        .meta
        .output_action_index(0)
        .ok_or(Error::Build("head index".into()))?;
    let head_cmx = tx::with_ironwood(&built.pczt, |b| {
        b.actions()[head_index].output().cmx().to_bytes()
    })?;
    let pczt = tx::prove(tx::io_finalize(built.pczt)?)?;
    let ask = SpendAuthorizingKey::from(funding_sk);
    let mut pczt = pczt;
    for i in spend_indices {
        pczt = tx::sign_with_ask(pczt, i, &ask)?;
    }
    let (raw_tx, txid) = tx::extract(pczt, params)?;
    // The head must be the only vault-addressed output, with the genesis memo.
    let outs = verify::vault_outputs(&raw_tx, app)?;
    if outs.len() != 1 || outs[0].cmx != head_cmx || outs[0].memo != memo || outs[0].value != float
    {
        return Err(Error::Verify(
            "genesis does not have exactly one well-formed head output".into(),
        ));
    }
    Ok(GenesisTx {
        raw_tx,
        txid,
        head_cmx,
        head_value: float,
        fee: built.fee,
        change_value: built.change_value,
    })
}
