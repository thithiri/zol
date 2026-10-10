//! Our FROST(Pallas, BLAKE2b-512) ciphersuite (`zol_zcash::redpallas`, frost-core 3.0 on
//! pasta_curves 0.6) against reddsa 0.5.2's `frost::redpallas` (pasta_curves 0.5), the
//! implementation ZOL used before the NU7 port. On fixed seeds every artefact must be
//! byte-identical: dealer and DKG key packages, identifiers, nonces and commitments,
//! signing packages, randomizers (explicit and seed-derived), signature shares and
//! aggregated signatures. Cross-deserialisation works too, because frost-core binds the
//! ciphersuite ID into every encoding.

use std::collections::BTreeMap;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use reddsa05::frost::redpallas as r;
use zol_zcash::{frost as f, redpallas as z};

const SEEDS: u64 = 12;

fn rng(seed: u64) -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(seed)
}

/// Our key packages and public package, re-read as reddsa's types from their bytes.
fn as_reddsa(
    kps: &f::KeyPackages,
    pk: &f::PublicKeyPackage,
) -> (
    BTreeMap<r::Identifier, r::keys::KeyPackage>,
    r::keys::PublicKeyPackage,
) {
    let kps = kps
        .iter()
        .map(|(i, kp)| {
            let rkp = r::keys::KeyPackage::deserialize(&kp.serialize().unwrap()).unwrap();
            (r::Identifier::deserialize(&i.serialize()).unwrap(), rkp)
        })
        .collect();
    let pk = r::keys::PublicKeyPackage::deserialize(&pk.serialize().unwrap()).unwrap();
    (kps, pk)
}

#[test]
fn dealer_keys_are_byte_identical() {
    for seed in 0..SEEDS {
        for (n, t) in [(3u16, 2u16), (5, 3), (2, 2)] {
            let (ours, ours_pk) = f::dealer(n, t, &mut rng(seed)).unwrap();
            let (theirs, theirs_pk) =
                r::keys::generate_with_dealer(n, t, r::keys::IdentifierList::Default, rng(seed))
                    .unwrap();
            assert_eq!(
                ours_pk.serialize().unwrap(),
                theirs_pk.serialize().unwrap(),
                "public key package, seed {seed}"
            );
            assert_eq!(ours.len(), theirs.len());
            for ((i, kp), (j, share)) in ours.iter().zip(theirs) {
                assert_eq!(i.serialize(), j.serialize());
                let theirs_kp = r::keys::KeyPackage::try_from(share).unwrap();
                assert_eq!(kp.serialize().unwrap(), theirs_kp.serialize().unwrap());
            }
            // Raw secret shares (with the VSS commitment, even-Y adjusted) too.
            let (ours_ss, _) = z::keys::generate_with_dealer(
                n,
                t,
                z::keys::IdentifierList::Default,
                &mut rng(seed),
            )
            .unwrap();
            let (theirs_ss, _) =
                r::keys::generate_with_dealer(n, t, r::keys::IdentifierList::Default, rng(seed))
                    .unwrap();
            for ((_, a), (_, b)) in ours_ss.iter().zip(&theirs_ss) {
                assert_eq!(a.serialize().unwrap(), b.serialize().unwrap());
            }
        }
    }
}

/// reddsa 0.5.2's DKG, run the way `zol_zcash::frost::dkg_local` runs ours.
fn reddsa_dkg(
    n: u16,
    t: u16,
    rng: &mut ChaCha20Rng,
) -> (
    BTreeMap<r::Identifier, r::keys::KeyPackage>,
    r::keys::PublicKeyPackage,
) {
    use r::keys::dkg;
    let ids: Vec<r::Identifier> = (1..=n)
        .map(|i| r::Identifier::try_from(i).unwrap())
        .collect();
    let mut r1s = BTreeMap::new();
    let mut r1p = BTreeMap::new();
    for &i in &ids {
        let (s, p) = dkg::part1(i, n, t, &mut *rng).unwrap();
        r1s.insert(i, s);
        r1p.insert(i, p);
    }
    let others = |i: r::Identifier| -> BTreeMap<_, _> {
        r1p.iter()
            .filter(|(k, _)| **k != i)
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    };
    let mut r2s = BTreeMap::new();
    let mut inbox: BTreeMap<r::Identifier, BTreeMap<r::Identifier, dkg::round2::Package>> =
        BTreeMap::new();
    for &i in &ids {
        let (s, out) = dkg::part2(r1s.remove(&i).unwrap(), &others(i)).unwrap();
        r2s.insert(i, s);
        for (to, pkg) in out {
            inbox.entry(to).or_default().insert(i, pkg);
        }
    }
    let mut kps = BTreeMap::new();
    let mut pk = None;
    for &i in &ids {
        let (kp, p) = dkg::part3(&r2s[&i], &others(i), &inbox[&i]).unwrap();
        pk = Some(p);
        kps.insert(i, kp);
    }
    (kps, pk.unwrap())
}

#[test]
fn dkg_keys_are_byte_identical() {
    for seed in 0..SEEDS {
        let (ours, ours_pk) = f::dkg_local(3, 2, &mut rng(seed)).unwrap();
        let (theirs, theirs_pk) = reddsa_dkg(3, 2, &mut rng(seed));
        assert_eq!(
            ours_pk.serialize().unwrap(),
            theirs_pk.serialize().unwrap(),
            "seed {seed}"
        );
        for ((_, a), (_, b)) in ours.iter().zip(&theirs) {
            assert_eq!(a.serialize().unwrap(), b.serialize().unwrap());
        }
        // ỹ(ak_group) = 0 after the even-Y normalisation.
        assert_eq!(f::group_key(&ours_pk)[31] & 0x80, 0);
    }
}

#[test]
fn identifiers_are_byte_identical() {
    for name in [&b"op0"[..], b"operator-1", b"", &[0xff; 40]] {
        assert_eq!(
            z::Identifier::derive(name).unwrap().serialize(),
            r::Identifier::derive(name).unwrap().serialize()
        );
    }
    for i in 1..=5u16 {
        assert_eq!(
            f::identifier(i).serialize(),
            r::Identifier::try_from(i).unwrap().serialize()
        );
    }
}

#[test]
#[allow(deprecated)]
fn rerandomized_signing_is_byte_identical() {
    for seed in 0..SEEDS {
        let (kps, pk) = f::dealer(3, 2, &mut rng(seed)).unwrap();
        let (rkps, rpk) = as_reddsa(&kps, &pk);
        let msg = [seed as u8; 32];
        // A random canonical scalar as `alpha` (the PCZT's randomizer): a fresh share.
        let alpha: [u8; 32] = {
            let (k, _) = f::dealer(2, 2, &mut rng(2000 + seed)).unwrap();
            k.values()
                .next()
                .unwrap()
                .signing_share()
                .serialize()
                .try_into()
                .unwrap()
        };
        for signers in [[1u16, 2], [1, 3], [2, 3]] {
            let mut nonce_rng_a = rng(5000 + seed);
            let mut nonce_rng_b = rng(5000 + seed);
            let mut comms = BTreeMap::new();
            let mut rcomms = BTreeMap::new();
            let mut nonces = BTreeMap::new();
            let mut rnonces = BTreeMap::new();
            for s in signers {
                let i = f::identifier(s);
                let ri = r::Identifier::try_from(s).unwrap();
                let (n, c) = f::commit(&kps[&i], &mut nonce_rng_a);
                let (rn, rc) = r::round1::commit(rkps[&ri].signing_share(), &mut nonce_rng_b);
                assert_eq!(n.serialize().unwrap(), rn.serialize().unwrap());
                assert_eq!(c.serialize().unwrap(), rc.serialize().unwrap());
                comms.insert(i, c);
                rcomms.insert(ri, rc);
                nonces.insert(i, n);
                rnonces.insert(ri, rn);
            }
            let sp = f::SigningPackage::new(comms, &msg);
            let rsp = r::SigningPackage::new(rcomms, &msg);
            assert_eq!(sp.serialize().unwrap(), rsp.serialize().unwrap());

            // Explicit randomizer (`alpha`), the path ZOL uses.
            let mut shares = BTreeMap::new();
            let mut rshares = BTreeMap::new();
            for s in signers {
                let i = f::identifier(s);
                let ri = r::Identifier::try_from(s).unwrap();
                let sh = f::sign_share(&sp, &nonces[&i], &kps[&i], &alpha).unwrap();
                let rsh = frost_rerandomized::sign::<r::PallasBlake2b512>(
                    &rsp,
                    &rnonces[&ri],
                    &rkps[&ri],
                    frost_rerandomized::Randomizer::deserialize(&alpha).unwrap(),
                )
                .unwrap();
                assert_eq!(sh.serialize(), rsh.serialize());
                shares.insert(i, sh);
                rshares.insert(ri, rsh);
            }
            let sig = f::aggregate(&sp, &shares, &pk, &alpha).unwrap();
            let rparams =
                frost_rerandomized::RandomizedParams::<r::PallasBlake2b512>::from_randomizer(
                    rpk.verifying_key(),
                    frost_rerandomized::Randomizer::deserialize(&alpha).unwrap(),
                );
            let rsig = r::rerandomized::aggregate(&rsp, &rshares, &rpk, &rparams).unwrap();
            assert_eq!(sig.to_vec(), rsig.serialize().unwrap());

            // Seed-derived randomizer (H "FROST_RedPallasA").
            let seed_bytes = [seed as u8 ^ 0x5a; 32];
            let ours = z::RandomizedParams::regenerate_from_seed_and_commitments(
                pk.verifying_key(),
                &seed_bytes,
                sp.signing_commitments(),
            )
            .unwrap();
            let theirs = frost_rerandomized::RandomizedParams::<r::PallasBlake2b512>::regenerate_from_seed_and_commitments(
                rpk.verifying_key(),
                &seed_bytes,
                rsp.signing_commitments(),
            )
            .unwrap();
            assert_eq!(
                ours.randomizer().serialize(),
                theirs.randomizer().serialize()
            );
            assert_eq!(
                ours.randomized_verifying_key().serialize().unwrap(),
                theirs.randomized_verifying_key().serialize().unwrap()
            );
        }
    }
}

#[test]
fn plain_frost_signing_is_byte_identical() {
    // H1-H5 without rerandomization (frost-core round2/aggregate).
    let (kps, pk) = f::dealer(3, 2, &mut rng(77)).unwrap();
    let (rkps, rpk) = as_reddsa(&kps, &pk);
    let msg = b"plain frost";
    let mut a = rng(78);
    let mut b = rng(78);
    let mut comms = BTreeMap::new();
    let mut rcomms = BTreeMap::new();
    let mut nonces = BTreeMap::new();
    let mut rnonces = BTreeMap::new();
    for (i, kp) in &kps {
        let ri = r::Identifier::deserialize(&i.serialize()).unwrap();
        let (n, c) = f::commit(kp, &mut a);
        let (rn, rc) = r::round1::commit(rkps[&ri].signing_share(), &mut b);
        comms.insert(*i, c);
        rcomms.insert(ri, rc);
        nonces.insert(*i, n);
        rnonces.insert(ri, rn);
    }
    let sp = f::SigningPackage::new(comms, msg);
    let rsp = r::SigningPackage::new(rcomms, msg);
    let mut shares = BTreeMap::new();
    let mut rshares = BTreeMap::new();
    for (i, kp) in &kps {
        let ri = r::Identifier::deserialize(&i.serialize()).unwrap();
        let sh = frost_core::round2::sign(&sp, &nonces[i], kp).unwrap();
        let rsh = r::round2::sign(&rsp, &rnonces[&ri], &rkps[&ri]).unwrap();
        assert_eq!(sh.serialize(), rsh.serialize());
        shares.insert(*i, sh);
        rshares.insert(ri, rsh);
    }
    let sig = frost_core::aggregate(&sp, &shares, &pk).unwrap();
    let rsig = r::aggregate(&rsp, &rshares, &rpk).unwrap();
    assert_eq!(sig.serialize().unwrap(), rsig.serialize().unwrap());
}
