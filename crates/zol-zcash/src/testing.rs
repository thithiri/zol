//! Test and tooling helpers: a single-key "group" (1-of-1) and plain wallet sends.

use ff::{Field, PrimeField};
use group::GroupEncoding;
use orchard::{
    keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey},
    tree::MerklePath,
    Anchor, Note,
};
use pasta_curves::pallas;
use rand::RngCore;

use crate::{
    keys::{spend_auth_generator, AppKeys, Receiver},
    params::ChainParams,
    tx::{self, ChangeSpec, OutputSpec, SpendSpec},
    Error,
};

/// A single-key vault group: `(ask_group, ak_group)` with ỹ(ak_group) = 0.
pub fn single_key_group(rng: &mut impl RngCore) -> ([u8; 32], [u8; 32]) {
    let mut s = pallas::Scalar::random(&mut *rng);
    let mut p = spend_auth_generator() * s;
    if p.to_bytes()[31] >> 7 == 1 {
        s = -s;
        p = -p;
    }
    (s.to_repr(), p.to_bytes())
}

/// The app's spend-authorizing scalar for a single-key group: `ask_group + t_app`.
pub fn app_ask(ask_group: &[u8; 32], keys: &AppKeys) -> [u8; 32] {
    (pallas::Scalar::from_repr(*ask_group).unwrap() + keys.tweak_scalar()).to_repr()
}

pub fn random_spending_key(rng: &mut impl RngCore) -> SpendingKey {
    loop {
        let mut b = [0u8; 32];
        rng.fill_bytes(&mut b);
        if let Some(sk) = Option::from(SpendingKey::from_bytes(b)) {
            return sk;
        }
    }
}

/// A note for `sk`'s external address that exists only off-chain, in a one-leaf tree:
/// `(note, path, anchor)`. Spends of it are fully valid transactions except that the
/// anchor is not a real chain state, which is enough for offline scanner tests.
pub fn offline_note(sk: &SpendingKey, value: u64) -> (Note, MerklePath, Anchor) {
    use incrementalmerkletree::{Hashable, Level};
    use orchard::{
        builder::{Builder, BundleType},
        bundle::BundleVersion,
        note::ExtractedNoteCommitment,
        tree::MerkleHashOrchard,
        value::NoteValue,
    };
    let fvk = FullViewingKey::from(sk);
    let bv = BundleVersion::ironwood_v3();
    let mut b = Builder::new(
        BundleType::DEFAULT,
        bv,
        bv.default_flags(),
        Anchor::empty_tree(),
    )
    .unwrap();
    b.add_output(
        None,
        fvk.address_at(0u32, Scope::External),
        NoteValue::from_raw(value),
        tx::empty_memo(),
    )
    .unwrap();
    let (bundle, meta) = b.build::<i64>(&mut rand::rngs::OsRng).unwrap().unwrap();
    let a = &bundle.actions()[meta.output_action_index(0).unwrap()];
    let (note, _, _) = zcash_note_encryption::try_note_decryption(
        &orchard::note_encryption::IronwoodDomain::for_action(a),
        &fvk.to_ivk(Scope::External).prepare(),
        a,
    )
    .unwrap();
    let path = MerklePath::from_parts(
        0,
        core::array::from_fn(|i| MerkleHashOrchard::empty_root(Level::from(i as u8))),
    );
    let anchor = path.root(ExtractedNoteCommitment::from(note.commitment()));
    (note, path, anchor)
}

/// An ordinary wallet send: spends `note` (owned by `sk`), pays `to` `value` with `memo`,
/// change to the wallet's internal address. Proved, signed and extracted.
#[allow(clippy::too_many_arguments)]
pub fn wallet_send(
    params: &ChainParams,
    sk: &SpendingKey,
    note: &Note,
    path: &MerklePath,
    anchor: Anchor,
    to: &Receiver,
    value: u64,
    memo: [u8; 512],
    target_height: u32,
) -> Result<(Vec<u8>, [u8; 32]), Error> {
    wallet_send_multi(
        params,
        sk,
        &[(*note, path.clone())],
        anchor,
        to,
        value,
        memo,
        target_height,
    )
}

/// [`wallet_send`] from several notes of `sk`, all witnessed to the same `anchor`.
/// ZIP 317 fee: 5,000 zat per action, at least 2 actions (actions = max(inputs, 2)).
#[allow(clippy::too_many_arguments)]
pub fn wallet_send_multi(
    params: &ChainParams,
    sk: &SpendingKey,
    inputs: &[(Note, MerklePath)],
    anchor: Anchor,
    to: &Receiver,
    value: u64,
    memo: [u8; 512],
    target_height: u32,
) -> Result<(Vec<u8>, [u8; 32]), Error> {
    if inputs.is_empty() {
        return Err(Error::Build("no inputs".into()));
    }
    let fvk = FullViewingKey::from(sk);
    let outputs = [OutputSpec {
        to: *to,
        value,
        ovk: Some(fvk.to_ovk(Scope::External)),
        memo,
    }];
    let change = ChangeSpec {
        to: fvk.address_at(0u32, Scope::Internal).to_raw_address_bytes(),
        ovk: Some(fvk.to_ovk(Scope::Internal)),
        memo: tx::empty_memo(),
    };
    let spends: Vec<SpendSpec> = inputs
        .iter()
        .map(|(note, path)| SpendSpec {
            fvk: fvk.clone(),
            note: *note,
            path: path.clone(),
        })
        .collect();
    let built = tx::build_ironwood(
        params,
        target_height,
        crate::DEFAULT_EXPIRY_DELTA,
        &spends,
        anchor,
        &outputs,
        Some(&change),
    )?;
    let idx = (0..spends.len())
        .map(|i| built.meta.spend_action_index(i))
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::Build("spend index".into()))?;
    let mut pczt = tx::prove(tx::io_finalize(built.pczt)?)?;
    let ask = SpendAuthorizingKey::from(sk);
    for i in idx {
        pczt = tx::sign_with_ask(pczt, i, &ask)?;
    }
    tx::extract(pczt, params)
}

/// ZIP 317 fee for `inputs` spends and `outputs` outputs in one Ironwood bundle.
pub fn zip317_fee(inputs: usize, outputs: usize) -> u64 {
    crate::ZIP317_MARGINAL_FEE * (inputs.max(outputs) as u64).max(crate::ZIP317_GRACE_ACTIONS)
}

/// Keys a `zol-zcashd --dev --dev-seed <seed>` process derives: a 2-of-3 dealer key set,
/// the master viewing secret and the dev wallet's spending key. Deterministic in `seed`.
pub struct DevSeedKeys {
    pub shares: crate::frost::KeyPackages,
    pub pubkeys: crate::frost::PublicKeyPackage,
    pub master_view: [u8; 32],
    pub wallet: SpendingKey,
}

pub fn dev_seed_keys(seed: &str) -> DevSeedKeys {
    use rand::SeedableRng;
    let h = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZOL_dev_seed_v1_")
        .hash(seed.as_bytes());
    let mut rng = rand_chacha::ChaCha20Rng::from_seed(h.as_bytes().try_into().unwrap());
    let (shares, pubkeys) = crate::frost::dealer(3, 2, &mut rng).expect("dealer");
    let mut master_view = [0u8; 32];
    rng.fill_bytes(&mut master_view);
    let wallet = random_spending_key(&mut rng);
    DevSeedKeys {
        shares,
        pubkeys,
        master_view,
        wallet,
    }
}

/// The dev wallet's Orchard-only unified address (external, index 0): what a node without
/// `generatetoaddress` (Zakura) must use as `mining.miner_address` for dev funding.
pub fn dev_wallet_ua(params: &ChainParams, seed: &str) -> String {
    let fvk = FullViewingKey::from(&dev_seed_keys(seed).wallet);
    crate::encode_ua(
        params,
        &fvk.address_at(0u32, Scope::External).to_raw_address_bytes(),
    )
}
