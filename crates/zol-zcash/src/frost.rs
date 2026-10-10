//! Rerandomized FROST over Pallas (ZIP 312) for vault spend authorization.
//!
//! reddsa 0.5.2 (`frost`), frost-rerandomized 3.0.0, frost-core 3.0.0. Signers hold *base*
//! shares from one key generation; app shares are derived on the fly by adding the app's
//! public tweak (spike 0.8). The randomizer is the PCZT's `alpha` (fixed by the builder
//! before round 1), so the hedged commitment-derived randomizer of frost-rerandomized 3.x is
//! not usable; see spike 0.7.
//!
//! Distributed use: each signer calls [`commit`] (round 1), the coordinator builds a
//! [`SigningPackage`] over the sighash, each signer calls [`sign_share`] after checking the
//! transaction itself, and anyone holding the shares and `alpha` calls [`aggregate`].
//! [`sign_locally`] runs all of that in one process (tests, tooling).

use std::collections::BTreeMap;

use ff::PrimeField;
use pasta_curves::pallas;
use rand_core::{CryptoRng, RngCore};
use reddsa::frost::redpallas::{self as rp, keys, keys::dkg, round1};
pub use reddsa::frost::redpallas::{
    keys::{KeyPackage, PublicKeyPackage, SigningShare, VerifyingShare},
    round1::{SigningCommitments, SigningNonces},
    round2::SignatureShare,
    Identifier, PallasBlake2b512, SigningPackage, VerifyingKey,
};

use crate::keys::{spend_auth_generator, AppKeys};
use crate::Error;

pub type KeyPackages = BTreeMap<Identifier, KeyPackage>;
type Randomizer = frost_rerandomized::Randomizer<PallasBlake2b512>;
type RandomizedParams = frost_rerandomized::RandomizedParams<PallasBlake2b512>;

pub fn identifier(i: u16) -> Identifier {
    Identifier::try_from(i).expect("non-zero")
}

/// The FROST group key bytes (`ak_group`), ỹ = 0 after reddsa's normalization.
pub fn group_key(pubkeys: &PublicKeyPackage) -> [u8; 32] {
    pubkeys
        .verifying_key()
        .serialize()
        .expect("non-identity")
        .try_into()
        .expect("32 bytes")
}

/// Trusted-dealer key generation (tests and tooling only).
pub fn dealer(
    max_signers: u16,
    min_signers: u16,
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<(KeyPackages, PublicKeyPackage), Error> {
    let (shares, pubkeys) = keys::generate_with_dealer(
        max_signers,
        min_signers,
        keys::IdentifierList::Default,
        &mut *rng,
    )
    .map_err(frost_err)?;
    let kps = shares
        .into_iter()
        .map(|(i, s)| Ok((i, KeyPackage::try_from(s).map_err(frost_err)?)))
        .collect::<Result<_, Error>>()?;
    Ok((kps, pubkeys))
}

/// A full DKG with every participant simulated in this process (tests and tooling only;
/// real operators run `reddsa::frost::redpallas::keys::dkg` across machines).
pub fn dkg_local(
    max_signers: u16,
    min_signers: u16,
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<(KeyPackages, PublicKeyPackage), Error> {
    let ids: Vec<_> = (1..=max_signers).map(identifier).collect();
    let mut r1s = BTreeMap::new();
    let mut r1p = BTreeMap::new();
    for &i in &ids {
        let (s, p) = dkg::part1(i, max_signers, min_signers, &mut *rng).map_err(frost_err)?;
        r1s.insert(i, s);
        r1p.insert(i, p);
    }
    let others = |i: Identifier| -> BTreeMap<_, _> {
        r1p.iter()
            .filter(|(k, _)| **k != i)
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    };
    let mut r2s = BTreeMap::new();
    let mut inbox: BTreeMap<Identifier, BTreeMap<Identifier, dkg::round2::Package>> =
        BTreeMap::new();
    for &i in &ids {
        let (s, out) = dkg::part2(r1s.remove(&i).unwrap(), &others(i)).map_err(frost_err)?;
        r2s.insert(i, s);
        for (to, pkg) in out {
            inbox.entry(to).or_default().insert(i, pkg);
        }
    }
    let mut kps = BTreeMap::new();
    let mut pk = None;
    for &i in &ids {
        let (kp, p) = dkg::part3(&r2s[&i], &others(i), &inbox[&i]).map_err(frost_err)?;
        if let Some(prev) = &pk {
            if prev != &p {
                return Err(Error::Frost(
                    "participants disagree on the public key package".into(),
                ));
            }
        }
        pk = Some(p);
        kps.insert(i, kp);
    }
    Ok((kps, pk.unwrap()))
}

/// A base share shifted by the app tweak: `s_i + t`, `Y_i + [t]G`, `Y + [t]G`.
pub fn tweak_key_package(kp: &KeyPackage, tweak: &[u8; 32]) -> Result<KeyPackage, Error> {
    let t = Option::<pallas::Scalar>::from(pallas::Scalar::from_repr(*tweak))
        .ok_or(Error::Key("bad tweak"))?;
    let tg = spend_auth_generator() * t;
    Ok(KeyPackage::new(
        *kp.identifier(),
        SigningShare::new(kp.signing_share().to_scalar() + t),
        VerifyingShare::new(kp.verifying_share().to_element() + tg),
        VerifyingKey::new(kp.verifying_key().to_element() + tg),
        *kp.min_signers(),
    ))
}

pub fn tweak_public_package(
    pk: &PublicKeyPackage,
    tweak: &[u8; 32],
) -> Result<PublicKeyPackage, Error> {
    let t = Option::<pallas::Scalar>::from(pallas::Scalar::from_repr(*tweak))
        .ok_or(Error::Key("bad tweak"))?;
    let tg = spend_auth_generator() * t;
    let vs = pk
        .verifying_shares()
        .iter()
        .map(|(i, v)| (*i, VerifyingShare::new(v.to_element() + tg)))
        .collect();
    Ok(PublicKeyPackage::new(
        vs,
        VerifyingKey::new(pk.verifying_key().to_element() + tg),
        pk.min_signers(),
    ))
}

/// App key package and public package for `app`, checked against `app.ak`.
pub fn app_packages(
    app: &AppKeys,
    kp: &KeyPackage,
    pk: &PublicKeyPackage,
) -> Result<(KeyPackage, PublicKeyPackage), Error> {
    let akp = tweak_key_package(kp, &app.tweak)?;
    let apk = tweak_public_package(pk, &app.tweak)?;
    if group_key(&apk) != app.ak {
        return Err(Error::Key(
            "base public package does not match this app's ak_group",
        ));
    }
    Ok((akp, apk))
}

/// Round 1 for one signer (use the *app* key package).
pub fn commit(
    app_kp: &KeyPackage,
    rng: &mut (impl RngCore + CryptoRng),
) -> (SigningNonces, SigningCommitments) {
    round1::commit(app_kp.signing_share(), rng)
}

/// Round 2 for one signer: a rerandomized share with randomizer `alpha` (the action's
/// PCZT `alpha`, little-endian scalar bytes). The caller must have checked the transaction
/// and computed `signing_package`'s message (the sighash) itself.
#[allow(deprecated)]
pub fn sign_share(
    signing_package: &SigningPackage,
    nonces: &SigningNonces,
    app_kp: &KeyPackage,
    alpha: &[u8; 32],
) -> Result<SignatureShare, Error> {
    let a = Option::<pallas::Scalar>::from(pallas::Scalar::from_repr(*alpha))
        .ok_or(Error::Frost("bad alpha".into()))?;
    frost_rerandomized::sign(signing_package, nonces, app_kp, Randomizer::from_scalar(a))
        .map_err(frost_err)
}

/// Aggregates shares into a 64-byte RedPallas spend-auth signature valid under
/// `rk = ak_app + [alpha]G`, and verifies it.
pub fn aggregate(
    signing_package: &SigningPackage,
    shares: &BTreeMap<Identifier, SignatureShare>,
    app_pk: &PublicKeyPackage,
    alpha: &[u8; 32],
) -> Result<[u8; 64], Error> {
    let a = Option::<pallas::Scalar>::from(pallas::Scalar::from_repr(*alpha))
        .ok_or(Error::Frost("bad alpha".into()))?;
    let params =
        RandomizedParams::from_randomizer(app_pk.verifying_key(), Randomizer::from_scalar(a));
    let sig =
        rp::rerandomized::aggregate(signing_package, shares, app_pk, &params).map_err(frost_err)?;
    params
        .randomized_verifying_key()
        .verify(signing_package.message(), &sig)
        .map_err(frost_err)?;
    Ok(sig
        .serialize()
        .map_err(frost_err)?
        .try_into()
        .expect("64 bytes"))
}

/// All rounds in one process, for the app key packages of `signers`.
pub fn sign_locally(
    app_kps: &KeyPackages,
    app_pk: &PublicKeyPackage,
    signers: &[Identifier],
    alpha: &[u8; 32],
    msg: &[u8],
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<[u8; 64], Error> {
    let mut nonces = BTreeMap::new();
    let mut comms = BTreeMap::new();
    for i in signers {
        let kp = app_kps
            .get(i)
            .ok_or(Error::Frost(format!("no key package for {i:?}")))?;
        let (n, c) = commit(kp, rng);
        nonces.insert(*i, n);
        comms.insert(*i, c);
    }
    let sp = SigningPackage::new(comms, msg);
    let mut shares = BTreeMap::new();
    for i in signers {
        shares.insert(*i, sign_share(&sp, &nonces[i], &app_kps[i], alpha)?);
    }
    aggregate(&sp, &shares, app_pk, alpha)
}

fn frost_err(e: impl core::fmt::Debug) -> Error {
    Error::Frost(format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Network;
    use ff::Field;
    use group::GroupEncoding;

    #[test]
    fn tweaked_threshold_signing() {
        let mut rng = rand::rngs::OsRng;
        let (kps, pk) = dkg_local(3, 2, &mut rng).unwrap();
        let app = AppKeys::derive(group_key(&pk), &[9; 32], &[0xA1; 32], Network::Regtest).unwrap();
        let mut akps = KeyPackages::new();
        let mut apk = None;
        for (i, kp) in &kps {
            let (a, p) = app_packages(&app, kp, &pk).unwrap();
            akps.insert(*i, a);
            apk = Some(p);
        }
        let apk = apk.unwrap();
        let msg = [7u8; 32];
        for s in [[1, 2], [1, 3], [2, 3]] {
            let alpha = pallas::Scalar::random(&mut rng);
            let sig = sign_locally(
                &akps,
                &apk,
                &[identifier(s[0]), identifier(s[1])],
                &alpha.to_repr(),
                &msg,
                &mut rng,
            )
            .unwrap();
            let ak = Option::<pallas::Point>::from(pallas::Point::from_bytes(&app.ak)).unwrap();
            let rk: [u8; 32] = (ak + spend_auth_generator() * alpha).to_bytes();
            let rk = orchard::primitives::redpallas::VerificationKey::<
                orchard::primitives::redpallas::SpendAuth,
            >::try_from(rk)
            .unwrap();
            rk.verify(&msg, &orchard::primitives::redpallas::Signature::from(sig))
                .unwrap();
        }
        assert!(sign_locally(&akps, &apk, &[identifier(1)], &[1; 32], &msg, &mut rng).is_err());
    }
}
