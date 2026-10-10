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

/// Why [`select_inputs`] found no covering set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortfall {
    /// What the selection had to cover with `max_inputs` inputs: amount + ZIP 317 fee.
    pub need: u64,
    /// Sum and count of the candidates it could use (the largest `max_inputs`).
    pub have: u64,
    pub notes: usize,
}

/// Largest-first selection from `free` notes `(value, id)`: the fewest that cover `amount`
/// plus the ZIP 317 fee for that many inputs and `outputs` outputs, at most `max_inputs`.
pub fn select_inputs<T: Copy + Ord>(
    mut free: Vec<(u64, T)>,
    amount: u64,
    outputs: usize,
    max_inputs: usize,
) -> Result<Vec<T>, Shortfall> {
    free.sort_by(|a, b| b.cmp(a));
    free.truncate(max_inputs);
    let mut sum = 0u64;
    let mut take = vec![];
    for (v, id) in &free {
        sum += v;
        take.push(*id);
        if sum >= amount + crate::testing::zip317_fee(take.len(), outputs) {
            return Ok(take);
        }
    }
    Err(Shortfall {
        need: amount + crate::testing::zip317_fee(free.len().max(1), outputs),
        have: sum,
        notes: free.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_inputs_largest_first_with_fee() {
        // fee is 10,000 for up to 2 actions, 5,000 per action beyond.
        let free = vec![(30_000u64, 1u8), (100_000, 2), (50_000, 3)];
        assert_eq!(select_inputs(free.clone(), 90_000, 2, 40), Ok(vec![2]));
        assert_eq!(select_inputs(free.clone(), 95_000, 2, 40), Ok(vec![2, 3]));
        // 3 inputs: fee 15,000; 180,000 - 15,000 = 165,000 is the most it can pay.
        assert_eq!(select_inputs(free.clone(), 165_000, 2, 40), Ok(vec![2, 3, 1]));
        assert_eq!(
            select_inputs(free.clone(), 165_001, 2, 40),
            Err(Shortfall { need: 180_001, have: 180_000, notes: 3 })
        );
        // The input cap drops the smallest notes.
        assert_eq!(
            select_inputs(free, 140_000, 2, 1),
            Err(Shortfall { need: 150_000, have: 100_000, notes: 1 })
        );
        assert_eq!(
            select_inputs(Vec::<(u64, u8)>::new(), 1, 2, 40),
            Err(Shortfall { need: 10_001, have: 0, notes: 0 })
        );
    }
}
