//! Spike 0.7: rerandomized FROST over Pallas (ZIP 312) via `reddsa::frost::redpallas`
//! (reddsa 0.5.2, frost-rerandomized 3.0.0, frost-core 3.0.0).

use std::collections::BTreeMap;

use ff::{FromUniformBytes, PrimeField};
use orchard::keys::FullViewingKey;
use pasta_curves::pallas;
use rand_core::{CryptoRng, RngCore};
use reddsa::frost::redpallas::{
    self as rp,
    keys::{self, dkg, EvenY, KeyPackage, PublicKeyPackage},
    round1, round2, Identifier, PallasBlake2b512, SigningPackage,
};

pub type KeyPackages = BTreeMap<Identifier, KeyPackage>;
pub type Randomizer = frost_rerandomized::Randomizer<PallasBlake2b512>;
pub type RandomizedParams = frost_rerandomized::RandomizedParams<PallasBlake2b512>;

pub fn id(i: u16) -> Identifier {
    Identifier::try_from(i).unwrap()
}

/// 2-of-3 with a trusted dealer. `post_generate` (reddsa) already negates the key and
/// all shares if the group key has odd ỹ.
pub fn dealer_2of3(rng: &mut (impl RngCore + CryptoRng)) -> (KeyPackages, PublicKeyPackage) {
    let (shares, pubkeys) =
        keys::generate_with_dealer(3, 2, keys::IdentifierList::Default, &mut *rng).expect("dealer");
    let kps = shares
        .into_iter()
        .map(|(i, s)| (i, KeyPackage::try_from(s).expect("verify share")))
        .collect();
    (kps, pubkeys)
}

/// 2-of-3 DKG (three parts, all participants simulated locally). `post_dkg` (reddsa)
/// negates the key packages if the resulting group key has odd ỹ.
pub fn dkg_2of3(rng: &mut (impl RngCore + CryptoRng)) -> (KeyPackages, PublicKeyPackage) {
    let ids: Vec<_> = (1..=3).map(id).collect();
    let mut r1_secret = BTreeMap::new();
    let mut r1_pkgs = BTreeMap::new();
    for &i in &ids {
        let (s, p) = dkg::part1(i, 3, 2, &mut *rng).expect("part1");
        r1_secret.insert(i, s);
        r1_pkgs.insert(i, p);
    }
    let mut r2_secret = BTreeMap::new();
    let mut r2_inbox: BTreeMap<Identifier, BTreeMap<Identifier, dkg::round2::Package>> = BTreeMap::new();
    for &i in &ids {
        let others: BTreeMap<_, _> = r1_pkgs.iter().filter(|(k, _)| **k != i).map(|(k, v)| (*k, v.clone())).collect();
        let (s, out) = dkg::part2(r1_secret.remove(&i).unwrap(), &others).expect("part2");
        r2_secret.insert(i, s);
        for (to, pkg) in out {
            r2_inbox.entry(to).or_default().insert(i, pkg);
        }
    }
    let mut kps = BTreeMap::new();
    let mut pubkeys = None;
    for &i in &ids {
        let others: BTreeMap<_, _> = r1_pkgs.iter().filter(|(k, _)| **k != i).map(|(k, v)| (*k, v.clone())).collect();
        let (kp, pk) = dkg::part3(&r2_secret[&i], &others, &r2_inbox[&i]).expect("part3");
        if let Some(prev) = &pubkeys {
            assert_eq!(prev, &pk, "all participants agree on the public key package");
        }
        pubkeys = Some(pk);
        kps.insert(i, kp);
    }
    (kps, pubkeys.unwrap())
}

pub fn group_key_bytes(pubkeys: &PublicKeyPackage) -> [u8; 32] {
    pubkeys.verifying_key().serialize().unwrap().try_into().unwrap()
}

pub fn ytilde(bytes: &[u8; 32]) -> u8 {
    bytes[31] >> 7
}

/// ToBase / ToScalar of a 64-byte BLAKE2b output (as Orchard's PRF^expand outputs are used).
pub fn to_base(h: &[u8; 64]) -> pallas::Base {
    pallas::Base::from_uniform_bytes(h)
}
pub fn to_scalar(h: &[u8; 64]) -> pallas::Scalar {
    pallas::Scalar::from_uniform_bytes(h)
}

/// FVK = (ak, nk, rivk) with ak the FROST group (or app) key. `None` if ak has ỹ = 1 or
/// the derived ivk is 0/⊥ (Orchard's own validity rules, enforced by `from_bytes`).
pub fn fvk_from_parts(ak: &[u8; 32], nk: pallas::Base, rivk: pallas::Scalar) -> Option<FullViewingKey> {
    let mut b = [0u8; 96];
    b[..32].copy_from_slice(ak);
    b[32..64].copy_from_slice(&nk.to_repr());
    b[64..].copy_from_slice(&rivk.to_repr());
    FullViewingKey::from_bytes(&b)
}

/// Rerandomized FROST signing of `msg` by `signers`, with the randomizer fixed to `alpha`
/// (taken from the PCZT: the Constructor chose alpha and committed rk = ak + [alpha]G into
/// the transaction effects before any FROST round, so the randomizer cannot be derived
/// from the round-1 commitments as frost-rerandomized 3.0 now recommends).
#[allow(deprecated)]
pub fn sign(
    kps: &KeyPackages,
    pubkeys: &PublicKeyPackage,
    signers: &[Identifier],
    alpha: pallas::Scalar,
    msg: &[u8],
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<[u8; 64], String> {
    let mut nonces = BTreeMap::new();
    let mut commitments = BTreeMap::new();
    for i in signers {
        let (n, c) = round1::commit(kps[i].signing_share(), &mut *rng);
        nonces.insert(*i, n);
        commitments.insert(*i, c);
    }
    let sp = SigningPackage::new(commitments, msg);
    let randomizer = Randomizer::from_scalar(alpha);
    let mut shares: BTreeMap<Identifier, round2::SignatureShare> = BTreeMap::new();
    for i in signers {
        // Each signer receives alpha over the confidential channel (here: the PCZT).
        let share = frost_rerandomized::sign(&sp, &nonces[i], &kps[i], randomizer)
            .map_err(|e| format!("round2 {i:?}: {e:?}"))?;
        shares.insert(*i, share);
    }
    let params = RandomizedParams::from_randomizer(pubkeys.verifying_key(), randomizer);
    let sig = rp::rerandomized::aggregate(&sp, &shares, pubkeys, &params).map_err(|e| format!("aggregate: {e:?}"))?;
    params
        .randomized_verifying_key()
        .verify(msg, &sig)
        .map_err(|e| format!("frost verify: {e:?}"))?;
    Ok(sig.serialize().unwrap().try_into().unwrap())
}

/// Does this key package set have an even-ỹ group key (reddsa's `EvenY`)?
pub fn has_even_y(pubkeys: &PublicKeyPackage) -> bool {
    pubkeys.has_even_y()
}
