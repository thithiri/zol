//! The FROST(Pallas, BLAKE2b-512) ciphersuite (RedPallas, ZIP 312) on frost-core 3.0.
//!
//! reddsa 0.6 removed its `frost` feature, with the ciphersuites "to be moved to the
//! ZcashFoundation/frost repository", which has not published RedPallas yet. This module is
//! a port of reddsa 0.5.2's `frost::redpallas` onto pasta_curves 0.6, kept byte-identical:
//!
//! - the same `ID` ("FROST(Pallas, BLAKE2b-512)"), which frost-core folds into every
//!   serialized key package, so existing `[keys]` configs still load;
//! - the same hash personalisations: H1 `FROST_RedPallasR`, H2 `Zcash_RedPallasH` (RedPallas
//!   H*), H3 `FROST_RedPallasN`, H4 `FROST_RedPallasM`, H5 `FROST_RedPallasC`, HDKG
//!   `FROST_RedPallasD`, HID `FROST_RedPallasI`, randomizer `FROST_RedPallasA`; scalars
//!   are BLAKE2b-512 outputs reduced with `from_uniform_bytes`;
//! - the same generator (Orchard `SpendAuthSig` basepoint) and encodings;
//! - the same even-Y normalisation of generated and DKG keys (Orchard `ak` has ỹ = 0);
//! - the same RNG consumption: a random scalar reads eight `next_u64` words, as
//!   pasta_curves 0.5's `Field::random` did, so seeded key generation is reproducible.
//!
//! `tests/frost_equivalence.rs` checks all of it against reddsa 0.5.2 on fixed seeds.

#![allow(non_snake_case)]

use std::collections::BTreeMap;

use ff::{Field as _, FromUniformBytes, PrimeField};
use frost_core::{Ciphersuite, Field, FieldError, Group, GroupError};
use frost_rerandomized::RandomizedCiphersuite;
use group::{Group as _, GroupEncoding};
use pasta_curves::pallas;
use rand_core::{CryptoRng, RngCore};

use crate::keys::spend_auth_generator;

fn h_star(personal: &[u8; 16], m: &[u8]) -> pallas::Scalar {
    let h = blake2b_simd::Params::new()
        .hash_length(64)
        .personal(personal)
        .hash(m);
    pallas::Scalar::from_uniform_bytes(h.as_array())
}

fn blake2b_64(personal: &[u8; 16], m: &[u8]) -> [u8; 64] {
    *blake2b_simd::Params::new()
        .hash_length(64)
        .personal(personal)
        .hash(m)
        .as_array()
}

/// The scalar field of Pallas.
#[derive(Clone, Copy)]
pub struct PallasScalarField;

impl Field for PallasScalarField {
    type Scalar = pallas::Scalar;
    type Serialization = [u8; 32];

    fn zero() -> Self::Scalar {
        pallas::Scalar::ZERO
    }

    fn one() -> Self::Scalar {
        pallas::Scalar::ONE
    }

    fn invert(scalar: &Self::Scalar) -> Result<Self::Scalar, FieldError> {
        if *scalar == pallas::Scalar::ZERO {
            Err(FieldError::InvalidZeroScalar)
        } else {
            Ok(scalar.invert().unwrap())
        }
    }

    /// Eight `next_u64` words, little-endian, reduced mod q: what pasta_curves 0.5's
    /// `Field::random` (used by reddsa 0.5.2) did.
    fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Self::Scalar {
        let mut wide = [0u8; 64];
        for chunk in wide.chunks_exact_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        pallas::Scalar::from_uniform_bytes(&wide)
    }

    fn serialize(scalar: &Self::Scalar) -> Self::Serialization {
        scalar.to_repr()
    }

    fn little_endian_serialize(scalar: &Self::Scalar) -> Self::Serialization {
        scalar.to_repr()
    }

    fn deserialize(buf: &Self::Serialization) -> Result<Self::Scalar, FieldError> {
        Option::from(pallas::Scalar::from_repr(*buf)).ok_or(FieldError::MalformedScalar)
    }
}

/// The Pallas group with the Orchard spend-auth generator.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PallasGroup;

impl Group for PallasGroup {
    type Field = PallasScalarField;
    type Element = pallas::Point;
    type Serialization = [u8; 32];

    fn cofactor() -> pallas::Scalar {
        pallas::Scalar::ONE
    }

    fn identity() -> Self::Element {
        pallas::Point::identity()
    }

    fn generator() -> Self::Element {
        spend_auth_generator()
    }

    fn serialize(element: &Self::Element) -> Result<Self::Serialization, GroupError> {
        if bool::from(element.is_identity()) {
            return Err(GroupError::InvalidIdentityElement);
        }
        Ok(element.to_bytes())
    }

    fn deserialize(buf: &Self::Serialization) -> Result<Self::Element, GroupError> {
        match Option::<pallas::Point>::from(pallas::Point::from_bytes(buf)) {
            Some(p) if bool::from(p.is_identity()) => Err(GroupError::InvalidIdentityElement),
            Some(p) => Ok(p),
            None => Err(GroupError::MalformedElement),
        }
    }
}

/// FROST(Pallas, BLAKE2b-512).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PallasBlake2b512;

impl Ciphersuite for PallasBlake2b512 {
    const ID: &'static str = "FROST(Pallas, BLAKE2b-512)";

    type Group = PallasGroup;
    type HashOutput = [u8; 64];
    type SignatureSerialization = [u8; 64];

    fn H1(m: &[u8]) -> pallas::Scalar {
        h_star(b"FROST_RedPallasR", m)
    }

    fn H2(m: &[u8]) -> pallas::Scalar {
        h_star(b"Zcash_RedPallasH", m)
    }

    fn H3(m: &[u8]) -> pallas::Scalar {
        h_star(b"FROST_RedPallasN", m)
    }

    fn H4(m: &[u8]) -> [u8; 64] {
        blake2b_64(b"FROST_RedPallasM", m)
    }

    fn H5(m: &[u8]) -> [u8; 64] {
        blake2b_64(b"FROST_RedPallasC", m)
    }

    fn HDKG(m: &[u8]) -> Option<pallas::Scalar> {
        Some(h_star(b"FROST_RedPallasD", m))
    }

    fn HID(m: &[u8]) -> Option<pallas::Scalar> {
        Some(h_star(b"FROST_RedPallasI", m))
    }

    fn post_generate(
        secret_shares: BTreeMap<Identifier, keys::SecretShare>,
        public_key_package: keys::PublicKeyPackage,
    ) -> Result<
        (
            BTreeMap<Identifier, keys::SecretShare>,
            keys::PublicKeyPackage,
        ),
        Error,
    > {
        let is_even = keys::has_even_y(public_key_package.verifying_key());
        let pk = keys::public_package_into_even_y(public_key_package, is_even);
        let shares = secret_shares
            .into_iter()
            .map(|(i, s)| (i, keys::secret_share_into_even_y(s, is_even)))
            .collect();
        Ok((shares, pk))
    }

    fn post_dkg(
        key_package: keys::KeyPackage,
        public_key_package: keys::PublicKeyPackage,
    ) -> Result<(keys::KeyPackage, keys::PublicKeyPackage), Error> {
        let is_even = keys::has_even_y(public_key_package.verifying_key());
        Ok((
            keys::key_package_into_even_y(key_package, is_even),
            keys::public_package_into_even_y(public_key_package, is_even),
        ))
    }
}

impl RandomizedCiphersuite for PallasBlake2b512 {
    fn hash_randomizer(m: &[u8]) -> Option<pallas::Scalar> {
        Some(h_star(b"FROST_RedPallasA", m))
    }
}

type P = PallasBlake2b512;

pub type Error = frost_core::Error<P>;
pub type Identifier = frost_core::Identifier<P>;
pub type SigningPackage = frost_core::SigningPackage<P>;
pub type Signature = frost_core::Signature<P>;
pub type VerifyingKey = frost_core::VerifyingKey<P>;
pub type Randomizer = frost_rerandomized::Randomizer<P>;
pub type RandomizedParams = frost_rerandomized::RandomizedParams<P>;

pub mod keys {
    use super::*;

    pub type SecretShare = frost_core::keys::SecretShare<P>;
    pub type SigningShare = frost_core::keys::SigningShare<P>;
    pub type VerifyingShare = frost_core::keys::VerifyingShare<P>;
    pub type KeyPackage = frost_core::keys::KeyPackage<P>;
    pub type PublicKeyPackage = frost_core::keys::PublicKeyPackage<P>;
    pub type IdentifierList<'a> = frost_core::keys::IdentifierList<'a, P>;
    pub type VerifiableSecretSharingCommitment =
        frost_core::keys::VerifiableSecretSharingCommitment<P>;

    pub fn generate_with_dealer<R: RngCore + CryptoRng>(
        max_signers: u16,
        min_signers: u16,
        identifiers: IdentifierList,
        rng: &mut R,
    ) -> Result<(BTreeMap<Identifier, SecretShare>, PublicKeyPackage), Error> {
        frost_core::keys::generate_with_dealer(max_signers, min_signers, identifiers, rng)
    }

    /// ỹ = 0: the top bit of the compressed point encoding is clear.
    pub(super) fn has_even_y(vk: &VerifyingKey) -> bool {
        match vk.serialize() {
            Ok(b) => b[31] & 0x80 == 0,
            Err(_) => true,
        }
    }

    pub(super) fn public_package_into_even_y(
        pk: PublicKeyPackage,
        is_even: bool,
    ) -> PublicKeyPackage {
        if is_even {
            return pk;
        }
        let vs = pk
            .verifying_shares()
            .iter()
            .map(|(i, v)| (*i, VerifyingShare::new(-v.to_element())))
            .collect();
        PublicKeyPackage::new(
            vs,
            VerifyingKey::new(-pk.verifying_key().to_element()),
            pk.min_signers(),
        )
    }

    pub(super) fn key_package_into_even_y(kp: KeyPackage, is_even: bool) -> KeyPackage {
        if is_even {
            return kp;
        }
        KeyPackage::new(
            *kp.identifier(),
            SigningShare::new(-kp.signing_share().to_scalar()),
            VerifyingShare::new(-kp.verifying_share().to_element()),
            VerifyingKey::new(-kp.verifying_key().to_element()),
            *kp.min_signers(),
        )
    }

    pub(super) fn secret_share_into_even_y(s: SecretShare, is_even: bool) -> SecretShare {
        if is_even {
            return s;
        }
        let coefficients: Vec<_> = s
            .commitment()
            .coefficients()
            .iter()
            .map(|e| {
                <PallasGroup as Group>::serialize(&-e.value())
                    .expect("no coefficient commitment is the identity")
            })
            .collect();
        let commitment =
            VerifiableSecretSharingCommitment::deserialize(coefficients).expect("just serialized");
        SecretShare::new(
            *s.identifier(),
            SigningShare::new(-s.signing_share().to_scalar()),
            commitment,
        )
    }

    pub mod dkg {
        use super::*;

        pub mod round1 {
            use super::*;
            pub type SecretPackage = frost_core::keys::dkg::round1::SecretPackage<P>;
            pub type Package = frost_core::keys::dkg::round1::Package<P>;
        }

        pub mod round2 {
            use super::*;
            pub type SecretPackage = frost_core::keys::dkg::round2::SecretPackage<P>;
            pub type Package = frost_core::keys::dkg::round2::Package<P>;
        }

        pub fn part1<R: RngCore + CryptoRng>(
            identifier: Identifier,
            max_signers: u16,
            min_signers: u16,
            rng: &mut R,
        ) -> Result<(round1::SecretPackage, round1::Package), Error> {
            frost_core::keys::dkg::part1(identifier, max_signers, min_signers, rng)
        }

        pub fn part2(
            secret: round1::SecretPackage,
            round1_packages: &BTreeMap<Identifier, round1::Package>,
        ) -> Result<(round2::SecretPackage, BTreeMap<Identifier, round2::Package>), Error> {
            frost_core::keys::dkg::part2(secret, round1_packages)
        }

        pub fn part3(
            secret: &round2::SecretPackage,
            round1_packages: &BTreeMap<Identifier, round1::Package>,
            round2_packages: &BTreeMap<Identifier, round2::Package>,
        ) -> Result<(KeyPackage, PublicKeyPackage), Error> {
            frost_core::keys::dkg::part3(secret, round1_packages, round2_packages)
        }
    }
}

pub mod round1 {
    use super::*;

    pub type SigningNonces = frost_core::round1::SigningNonces<P>;
    pub type SigningCommitments = frost_core::round1::SigningCommitments<P>;

    pub fn commit<R: RngCore + CryptoRng>(
        secret: &keys::SigningShare,
        rng: &mut R,
    ) -> (SigningNonces, SigningCommitments) {
        frost_core::round1::commit::<P, R>(secret, rng)
    }
}

pub mod round2 {
    pub type SignatureShare = frost_core::round2::SignatureShare<super::P>;
}

/// Rerandomized aggregation (frost-rerandomized) with explicit [`RandomizedParams`].
pub fn aggregate_rerandomized(
    signing_package: &SigningPackage,
    shares: &BTreeMap<Identifier, round2::SignatureShare>,
    pubkeys: &keys::PublicKeyPackage,
    params: &RandomizedParams,
) -> Result<Signature, Error> {
    frost_rerandomized::aggregate::<P>(signing_package, shares, pubkeys, params)
}
