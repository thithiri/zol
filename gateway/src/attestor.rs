//! Threshold attestation (plan 3.5; design "Attestation and credit").
//!
//! The attestor key is a FROST(Ed25519, SHA-512) group key, so an aggregate
//! signature is a plain Ed25519 signature the ledger's precompile verifies.
//!
//! An operator releases a signature share only if
//! 1. the statement equals the one its own scan and ledger view produce, and
//! 2. it has durably recorded that decision, and has never recorded a
//!    conflicting one for the same deposit (credit vs void) or sequence.
//!
//! The ledger's single `Deposit` account is the guarantee; the log is a second line.

use std::collections::{BTreeMap, HashMap};

use frost_ed25519 as frost;
use frost::keys::{KeyPackage, PublicKeyPackage};
use frost::round1::{SigningCommitments, SigningNonces};
use frost::round2::SignatureShare;
use frost::{Identifier, SigningPackage};
use rand::{CryptoRng, RngCore};
use zol_wire::digest::{paid_digest, void_digest, Sha256};
use zol_wire::Network;

use crate::chain::AppId;
use crate::decide::Credit;
use crate::store::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    Credit { app: AppId, credit: Credit },
    Void { app: AppId, deposit_id: [u8; 32] },
    Paid { app: AppId, sequence: u64 },
}

impl Statement {
    pub fn digest(&self, network: Network) -> [u8; 32] {
        match self {
            Statement::Credit { app, credit } => credit.attestation(network, app).digest::<Sha256>(),
            Statement::Void { app, deposit_id } => void_digest::<Sha256>(network as u8, app, deposit_id),
            Statement::Paid { app, sequence } => paid_digest::<Sha256>(network as u8, app, *sequence),
        }
    }

    /// Credit and void share a key, so the log refuses to sign both for one deposit.
    pub fn log_key(&self) -> Vec<u8> {
        match self {
            Statement::Credit { credit, .. } => [b"dep:".as_slice(), &credit.deposit_id].concat(),
            Statement::Void { deposit_id, .. } => [b"dep:".as_slice(), deposit_id].concat(),
            Statement::Paid { app, sequence } => [b"paid:".as_slice(), app, &sequence.to_le_bytes()].concat(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Statement::Credit { .. } => "credit",
            Statement::Void { .. } => "void",
            Statement::Paid { .. } => "paid",
        }
    }
}

/// An aggregate signature and the operators that refused to contribute.
pub type Attested = ([u8; 64], Vec<(Identifier, AttestError)>);

#[derive(Debug, thiserror::Error)]
pub enum AttestError {
    #[error("frost: {0}")]
    Frost(#[from] frost::Error),
    #[error("operator's own view differs from the requested statement")]
    Disagree,
    #[error("conflicting decision already signed")]
    Conflict,
    #[error("store: {0}")]
    Store(StoreError),
    #[error("signing package does not carry the statement digest")]
    WrongMessage,
    #[error("unknown or reused signing session")]
    NoNonces,
    #[error("fewer than threshold operators agreed: {0}")]
    BelowThreshold(usize),
}

impl From<StoreError> for AttestError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Conflict => AttestError::Conflict,
            e => AttestError::Store(e),
        }
    }
}

/// One operator's attestor key share.
pub struct Attestor {
    pub id: Identifier,
    key: KeyPackage,
    pubkeys: PublicKeyPackage,
    network: Network,
    nonces: HashMap<u64, SigningNonces>,
}

impl Attestor {
    pub fn new(key: KeyPackage, pubkeys: PublicKeyPackage, network: Network) -> Self {
        Self { id: *key.identifier(), key, pubkeys, network, nonces: HashMap::new() }
    }

    pub fn group_key(&self) -> [u8; 32] {
        group_key(&self.pubkeys)
    }

    /// Round 1. Nonces are kept per session and used at most once.
    pub fn commit<R: RngCore + CryptoRng>(&mut self, session: u64, rng: &mut R) -> SigningCommitments {
        let (nonces, commitments) = frost::round1::commit(self.key.signing_share(), rng);
        self.nonces.insert(session, nonces);
        commitments
    }

    /// Round 2. `own_view` is the statement this operator derives itself.
    pub fn sign(
        &mut self,
        session: u64,
        package: &SigningPackage,
        requested: &Statement,
        own_view: Option<&Statement>,
        log: &Store,
    ) -> Result<SignatureShare, AttestError> {
        let nonces = self.nonces.remove(&session).ok_or(AttestError::NoNonces)?;
        if own_view != Some(requested) {
            return Err(AttestError::Disagree);
        }
        let digest = requested.digest(self.network);
        if package.message() != digest.as_slice() {
            return Err(AttestError::WrongMessage);
        }
        log.record_decision(&requested.log_key(), requested.kind(), &digest)?;
        Ok(frost::round2::sign(package, &nonces, &self.key)?)
    }
}

pub fn group_key(pubkeys: &PublicKeyPackage) -> [u8; 32] {
    pubkeys.verifying_key().serialize().expect("serializable").try_into().expect("32 bytes")
}

/// Trusted-dealer key generation, for tests and local tooling.
pub fn dealer_keys<R: RngCore + CryptoRng>(n: u16, t: u16, rng: &mut R) -> (Vec<KeyPackage>, PublicKeyPackage) {
    let (shares, pubkeys) = frost::keys::generate_with_dealer(n, t, frost::keys::IdentifierList::Default, rng).unwrap();
    let keys = shares.into_values().map(|s| KeyPackage::try_from(s).unwrap()).collect();
    (keys, pubkeys)
}

/// Distributed key generation among `n` operators run in one process; each
/// operator's secrets only ever touch its own state. Production runs the same
/// three parts over an authenticated channel.
pub fn dkg<R: RngCore + CryptoRng>(n: u16, t: u16, rng: &mut R) -> Result<(Vec<KeyPackage>, PublicKeyPackage), frost::Error> {
    use frost::keys::dkg;
    let ids: Vec<Identifier> = (1..=n).map(|i| Identifier::try_from(i).unwrap()).collect();
    let mut s1 = BTreeMap::new();
    let mut p1 = BTreeMap::new();
    for id in &ids {
        let (sec, pkg) = dkg::part1(*id, n, t, &mut *rng)?;
        s1.insert(*id, sec);
        p1.insert(*id, pkg);
    }
    let mut s2 = BTreeMap::new();
    let mut p2: BTreeMap<Identifier, BTreeMap<Identifier, dkg::round2::Package>> = BTreeMap::new();
    for id in &ids {
        let others: BTreeMap<_, _> = p1.iter().filter(|(k, _)| *k != id).map(|(k, v)| (*k, v.clone())).collect();
        let (sec, out) = dkg::part2(s1.remove(id).unwrap(), &others)?;
        s2.insert(*id, sec);
        for (to, pkg) in out {
            p2.entry(to).or_default().insert(*id, pkg);
        }
    }
    let mut keys = Vec::new();
    let mut pubkeys = None;
    for id in &ids {
        let others: BTreeMap<_, _> = p1.iter().filter(|(k, _)| *k != id).map(|(k, v)| (*k, v.clone())).collect();
        let (kp, pk) = dkg::part3(&s2[id], &others, &p2[id])?;
        keys.push(kp);
        pubkeys = Some(pk);
    }
    Ok((keys, pubkeys.unwrap()))
}

/// One participant in a signing run: its attestor, its decision log, and the
/// statement its own view produces for this request.
pub struct Participant<'a> {
    pub attestor: &'a mut Attestor,
    pub log: &'a Store,
    pub own_view: Option<Statement>,
}

/// Runs FROST for `statement` with every participant that agrees, and
/// aggregates once `threshold` shares are in. Returns the 64-byte signature and
/// the operators that refused.
pub fn attest<R: RngCore + CryptoRng>(
    statement: &Statement,
    network: Network,
    pubkeys: &PublicKeyPackage,
    threshold: usize,
    participants: &mut [Participant<'_>],
    session: u64,
    rng: &mut R,
) -> Result<Attested, AttestError> {
    let digest = statement.digest(network);
    // Ask only operators whose own view matches and whose log does not already
    // hold a conflicting decision, so no nonces are wasted.
    let mut refused: Vec<(Identifier, AttestError)> = Vec::new();
    let mut willing: Vec<usize> = Vec::new();
    for (i, p) in participants.iter().enumerate() {
        if p.own_view.as_ref() != Some(statement) {
            refused.push((p.attestor.id, AttestError::Disagree));
            continue;
        }
        match p.log.decision(&statement.log_key()) {
            Ok(Some((k, d))) if k != statement.kind() || d != digest => refused.push((p.attestor.id, AttestError::Conflict)),
            Ok(_) => willing.push(i),
            Err(e) => refused.push((p.attestor.id, e.into())),
        }
    }
    // Rounds with replacement: an operator that refuses at signing time is
    // dropped and the next willing one takes its place.
    let mut session = session;
    loop {
        if willing.len() < threshold {
            return Err(AttestError::BelowThreshold(willing.len()));
        }
        let chosen: Vec<usize> = willing[..threshold].to_vec();
        let mut commitments = BTreeMap::new();
        for &i in &chosen {
            let p = &mut participants[i];
            commitments.insert(p.attestor.id, p.attestor.commit(session, rng));
        }
        let package = SigningPackage::new(commitments, &digest);
        let mut shares = BTreeMap::new();
        let mut dropped = Vec::new();
        for &i in &chosen {
            let p = &mut participants[i];
            match p.attestor.sign(session, &package, statement, p.own_view.as_ref(), p.log) {
                Ok(s) => {
                    shares.insert(p.attestor.id, s);
                }
                Err(e) => {
                    refused.push((p.attestor.id, e));
                    dropped.push(i);
                }
            }
        }
        if dropped.is_empty() {
            let sig = frost::aggregate(&package, &shares, pubkeys)?;
            pubkeys.verifying_key().verify(&digest, &sig)?;
            let bytes: [u8; 64] = sig.serialize()?.try_into().expect("64 bytes");
            return Ok((bytes, refused));
        }
        // Unused nonces of this session are discarded with it.
        for &i in &chosen {
            participants[i].attestor.nonces.remove(&session);
        }
        willing.retain(|i| !dropped.contains(i));
        session = session.wrapping_add(1 << 32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn void(id: u8) -> Statement {
        Statement::Void { app: [1; 32], deposit_id: [id; 32] }
    }

    #[test]
    fn frost_signature_is_plain_ed25519() {
        let (keys, pubkeys) = dkg(3, 2, &mut OsRng).unwrap();
        let mut ats: Vec<Attestor> = keys.into_iter().map(|k| Attestor::new(k, pubkeys.clone(), Network::Testnet)).collect();
        let logs: Vec<Store> = (0..3).map(|_| Store::in_memory().unwrap()).collect();
        let st = void(1);
        let mut parts: Vec<Participant> = ats
            .iter_mut()
            .zip(&logs)
            .map(|(a, l)| Participant { attestor: a, log: l, own_view: Some(st.clone()) })
            .collect();
        let (sig, refused) = attest(&st, Network::Testnet, &pubkeys, 2, &mut parts, 1, &mut OsRng).unwrap();
        assert!(refused.is_empty());
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&group_key(&pubkeys)).unwrap();
        vk.verify_strict(&st.digest(Network::Testnet), &ed25519_dalek::Signature::from_bytes(&sig)).unwrap();
    }

    #[test]
    fn operators_refuse_disagreement_and_conflicts() {
        let (keys, pubkeys) = dealer_keys(3, 2, &mut OsRng);
        let mut ats: Vec<Attestor> = keys.into_iter().map(|k| Attestor::new(k, pubkeys.clone(), Network::Testnet)).collect();
        let logs: Vec<Store> = (0..3).map(|_| Store::in_memory().unwrap()).collect();
        let st = void(2);
        // Only one operator sees a void: below threshold, nothing is signed.
        {
            let mut parts: Vec<Participant> = ats
                .iter_mut()
                .zip(&logs)
                .enumerate()
                .map(|(i, (a, l))| Participant { attestor: a, log: l, own_view: (i == 0).then(|| st.clone()) })
                .collect();
            assert!(matches!(
                attest(&st, Network::Testnet, &pubkeys, 2, &mut parts, 1, &mut OsRng),
                Err(AttestError::BelowThreshold(1))
            ));
        }
        // Operators 0 and 1 have already signed a credit for this deposit.
        let credit = Statement::Credit {
            app: [1; 32],
            credit: Credit {
                deposit_id: [2; 32],
                amount: 5,
                session_key: [3; 32],
                payout_commitment: [4; 32],
                flags: 0,
                pot_id: [0; 32],
                app_data: vec![],
            },
        };
        for l in &logs[..2] {
            l.record_decision(&credit.log_key(), "credit", &credit.digest(Network::Testnet)).unwrap();
        }
        let mut parts: Vec<Participant> = ats
            .iter_mut()
            .zip(&logs)
            .map(|(a, l)| Participant { attestor: a, log: l, own_view: Some(st.clone()) })
            .collect();
        let r = attest(&st, Network::Testnet, &pubkeys, 2, &mut parts, 2, &mut OsRng);
        assert!(matches!(r, Err(AttestError::BelowThreshold(_))), "{r:?}");
    }

    #[test]
    fn nonces_are_single_use() {
        let (keys, pubkeys) = dealer_keys(2, 2, &mut OsRng);
        let mut a = Attestor::new(keys[0].clone(), pubkeys.clone(), Network::Testnet);
        let mut b = Attestor::new(keys[1].clone(), pubkeys.clone(), Network::Testnet);
        let log = Store::in_memory().unwrap();
        let st = void(3);
        let mut m = BTreeMap::new();
        m.insert(a.id, a.commit(9, &mut OsRng));
        m.insert(b.id, b.commit(9, &mut OsRng));
        let pkg = SigningPackage::new(m, &st.digest(Network::Testnet));
        a.sign(9, &pkg, &st, Some(&st), &log).unwrap();
        assert!(matches!(a.sign(9, &pkg, &st, Some(&st), &log), Err(AttestError::NoNonces)));
        // A package over a different message is refused.
        let mut m = BTreeMap::new();
        m.insert(a.id, a.commit(10, &mut OsRng));
        m.insert(b.id, b.commit(10, &mut OsRng));
        let pkg = SigningPackage::new(m, &[0u8; 32]);
        assert!(matches!(a.sign(10, &pkg, &st, Some(&st), &log), Err(AttestError::WrongMessage)));
    }
}
