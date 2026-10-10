//! Attestor key generation across hosts (`zol-gateway dkg`).
//!
//! The three FROST(Ed25519) DKG parts of `attestor::dkg`, with each operator on
//! its own host and the messages carried over the signed peer channel
//! (`peer::verify_request`). Each host serves a small mailbox on its `listen`
//! address, pushes its own messages to every peer until they are accepted, and
//! waits for everyone else's:
//!
//! 1. `round1`: the operator's commitment package, plus a fresh X25519 key.
//! 2. `round2`: one secret package per recipient, encrypted to that recipient
//!    (ChaCha20-Poly1305 under X25519 between the two round-1 keys).
//! 3. `confirm`: the group key and a hash of every round-1 package received.
//!    FROST's DKG needs round 1 to be a broadcast; comparing transcripts catches
//!    an operator that sent different packages to different peers.
//!
//! Every message carries a context hash over the ceremony ID, the threshold
//! and the participants (rotation order and guardian keys), so hosts with
//! different configs refuse each other instead of producing unrelated keys.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use frost_ed25519::keys::dkg::{self, round1, round2};
use frost_ed25519::keys::{KeyPackage, PublicKeyPackage};
use frost_ed25519::Identifier;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tiny_http::{Header, Method, Response};

use crate::attestor::group_key;
use crate::peer::{now_secs, signing_message, verify_request, AuthHeaders};

#[derive(Debug, thiserror::Error)]
pub enum DkgError {
    #[error("config: {0}")]
    Config(String),
    #[error("frost: {0}")]
    Frost(#[from] frost_ed25519::Error),
    #[error("{0}")]
    Protocol(String),
    #[error("timed out waiting for {0}")]
    Timeout(String),
    #[error("io: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, DkgError>;

/// One participant, as every host lists it.
#[derive(Clone, Debug)]
pub struct Participant {
    pub name: String,
    /// Base URL of its mailbox (its gateway `listen` address, as reached from here).
    pub url: String,
    pub guardian: VerifyingKey,
}

pub struct DkgParams {
    /// Shared label for this ceremony; every host must use the same one.
    pub ceremony: String,
    pub threshold: u16,
    /// Every operator in rotation order, this one included. Position i gets
    /// FROST identifier i + 1.
    pub participants: Vec<Participant>,
    /// This host's name in `participants`.
    pub me: String,
    pub guardian_seed: [u8; 32],
    pub listen: String,
    pub timeout: Duration,
}

impl DkgParams {
    fn context(&self) -> [u8; 32] {
        let mut h = sha2::Sha256::new();
        h.update(b"zol-attestor-dkg-v1\n");
        h.update(self.ceremony.as_bytes());
        h.update([0]);
        h.update(self.threshold.to_le_bytes());
        h.update((self.participants.len() as u16).to_le_bytes());
        for p in &self.participants {
            h.update(p.name.as_bytes());
            h.update([0]);
            h.update(p.guardian.to_bytes());
        }
        h.finalize().into()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Envelope {
    context: [u8; 32],
    /// Sender's position in `participants`.
    from: u16,
    body: Body,
}

#[derive(Clone, Serialize, Deserialize)]
enum Body {
    Round1 { package: round1::Package, enc_key: [u8; 32] },
    Round2 { ciphertext: Vec<u8> },
    Confirm { group_key: [u8; 32], transcript: [u8; 32] },
}

#[derive(Default)]
struct Mailbox {
    round1: BTreeMap<u16, (round1::Package, [u8; 32])>,
    round2: BTreeMap<u16, Vec<u8>>,
    confirm: BTreeMap<u16, ([u8; 32], [u8; 32])>,
}

fn id(pos: u16) -> Identifier {
    Identifier::try_from(pos + 1).expect("nonzero")
}

/// Key for messages from `from` to `to`; distinct per direction.
fn pair_key(shared: &[u8; 32], context: &[u8; 32], from: u16, to: u16) -> Key {
    let k = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZOL_DkgRound2_v1")
        .to_state()
        .update(shared)
        .update(context)
        .update(&from.to_le_bytes())
        .update(&to.to_le_bytes())
        .finalize();
    *Key::from_slice(k.as_bytes())
}

fn json_resp(code: u16, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body).with_status_code(code).with_header(Header::from_bytes("content-type", "application/json").unwrap())
}

/// Runs this host's side of the ceremony. Returns its key package and the
/// group's public key package, identical on every host.
pub fn run<R: RngCore + CryptoRng>(p: &DkgParams, rng: &mut R) -> Result<(KeyPackage, PublicKeyPackage)> {
    let n = p.participants.len() as u16;
    if p.threshold < 2 || p.threshold > n {
        return Err(DkgError::Config(format!("threshold {} of {n}", p.threshold)));
    }
    let me = p.participants.iter().position(|q| q.name == p.me).ok_or_else(|| DkgError::Config(format!("{} not in participants", p.me)))? as u16;
    let key = SigningKey::from_bytes(&p.guardian_seed);
    if key.verifying_key() != p.participants[me as usize].guardian {
        return Err(DkgError::Config("guardian key does not match this host's entry".into()));
    }
    let context = p.context();
    let guardians: Vec<VerifyingKey> = p.participants.iter().map(|q| q.guardian).collect();

    // Mailbox server.
    let mailbox = Arc::new(Mutex::new(Mailbox::default()));
    let server = Arc::new(tiny_http::Server::http(&p.listen).map_err(|e| DkgError::Io(e.to_string()))?);
    let srv = {
        let (server, mailbox, guardians) = (server.clone(), mailbox.clone(), guardians.clone());
        std::thread::spawn(move || {
            let mut seen = std::collections::HashMap::new();
            for mut req in server.incoming_requests() {
                let mut body = Vec::new();
                let _ = req.as_reader().read_to_end(&mut body);
                let resp = receive(&req, &body, &guardians, &mut seen, &context, &mailbox);
                let _ = req.respond(resp);
            }
        })
    };
    let result = ceremony(p, me, n, context, &key, &mailbox, rng);
    // Stay reachable briefly, in case a peer retries a message we already took.
    if result.is_ok() {
        std::thread::sleep(Duration::from_secs(2));
    }
    server.unblock();
    let _ = srv.join();
    result
}

fn receive(
    req: &tiny_http::Request,
    body: &[u8],
    guardians: &[VerifyingKey],
    seen: &mut std::collections::HashMap<([u8; 32], [u8; 16]), i64>,
    context: &[u8; 32],
    mailbox: &Mutex<Mailbox>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    if *req.method() != Method::Post || req.url() != "/peer/dkg" {
        return json_resp(404, r#"{"error":"not found"}"#);
    }
    let h = |name: &'static str| req.headers().iter().find(|x| x.field.equiv(name)).map(|x| x.value.as_str()).unwrap_or("");
    let auth = AuthHeaders { peer: h("x-zol-peer"), ts: h("x-zol-ts"), nonce: h("x-zol-nonce"), sig: h("x-zol-sig") };
    let who = match verify_request(guardians, seen, "/peer/dkg", auth, body, now_secs()) {
        Ok(k) => k,
        Err(e) => return json_resp(401, &serde_json::to_string(&e).unwrap_or_default()),
    };
    let Ok(env) = serde_json::from_slice::<Envelope>(body) else { return json_resp(400, r#"{"error":"body"}"#) };
    if env.context != *context {
        return json_resp(409, r#"{"error":"different ceremony: check ceremony ID, threshold, order and guardian keys"}"#);
    }
    // The signer must be the participant the message claims to be from.
    if guardians.get(env.from as usize) != Some(&who) {
        return json_resp(403, r#"{"error":"sender is not that participant"}"#);
    }
    let mut m = mailbox.lock().unwrap();
    // First message per (sender, round) wins; a different repeat is refused.
    let conflict = match env.body {
        Body::Round1 { package, enc_key } => {
            let prev = m.round1.entry(env.from).or_insert((package.clone(), enc_key));
            prev.0 != package || prev.1 != enc_key
        }
        Body::Round2 { ciphertext } => *m.round2.entry(env.from).or_insert(ciphertext.clone()) != ciphertext,
        Body::Confirm { group_key, transcript } => *m.confirm.entry(env.from).or_insert((group_key, transcript)) != (group_key, transcript),
    };
    if conflict {
        return json_resp(409, r#"{"error":"conflicting message"}"#);
    }
    json_resp(200, "null")
}

/// Pushes one message to a peer until it is accepted or refused.
fn push(http: &reqwest::blocking::Client, key: &SigningKey, to: &Participant, env: &Envelope, deadline: Instant) -> Result<()> {
    let body = serde_json::to_vec(env).map_err(|e| DkgError::Io(e.to_string()))?;
    let url = format!("{}/peer/dkg", to.url.trim_end_matches('/'));
    loop {
        let ts = now_secs();
        let nonce = hex::encode(rand::random::<[u8; 16]>());
        let sig = key.sign(&signing_message("/peer/dkg", ts, &nonce, &body));
        let r = http
            .post(&url)
            .header("content-type", "application/json")
            .header("x-zol-peer", hex::encode(key.verifying_key().to_bytes()))
            .header("x-zol-ts", ts.to_string())
            .header("x-zol-nonce", nonce)
            .header("x-zol-sig", hex::encode(sig.to_bytes()))
            .body(body.clone())
            .send();
        match r {
            Ok(r) if r.status().is_success() => return Ok(()),
            // Unreachable, or not up yet: retry. Any other answer is final.
            Ok(r) if r.status().is_server_error() => {}
            Ok(r) => {
                let code = r.status();
                return Err(DkgError::Protocol(format!("{} refused: {code} {}", to.name, r.text().unwrap_or_default())));
            }
            Err(_) => {}
        }
        if Instant::now() > deadline {
            return Err(DkgError::Timeout(format!("{} to accept a message", to.name)));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn broadcast(p: &DkgParams, me: u16, key: &SigningKey, deadline: Instant, mk: impl Fn(u16) -> Result<Envelope>) -> Result<()> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().map_err(|e| DkgError::Io(e.to_string()))?;
    for (i, q) in p.participants.iter().enumerate() {
        if i as u16 != me {
            push(&http, key, q, &mk(i as u16)?, deadline)?;
        }
    }
    Ok(())
}

fn wait_for<T>(what: &str, deadline: Instant, mailbox: &Mutex<Mailbox>, f: impl Fn(&Mailbox) -> Option<T>) -> Result<T> {
    loop {
        if let Some(v) = f(&mailbox.lock().unwrap()) {
            return Ok(v);
        }
        if Instant::now() > deadline {
            return Err(DkgError::Timeout(what.into()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn ceremony<R: RngCore + CryptoRng>(
    p: &DkgParams,
    me: u16,
    n: u16,
    context: [u8; 32],
    key: &SigningKey,
    mailbox: &Mutex<Mailbox>,
    rng: &mut R,
) -> Result<(KeyPackage, PublicKeyPackage)> {
    let deadline = Instant::now() + p.timeout;
    let others = n - 1;

    // Round 1.
    let (s1, pkg1) = dkg::part1(id(me), n, p.threshold, &mut *rng)?;
    let enc_secret = x25519_dalek::StaticSecret::random_from_rng(&mut *rng);
    let enc_key = x25519_dalek::PublicKey::from(&enc_secret).to_bytes();
    let r1 = Body::Round1 { package: pkg1.clone(), enc_key };
    broadcast(p, me, key, deadline, |_| Ok(Envelope { context, from: me, body: r1.clone() }))?;
    let got1 = wait_for("round 1", deadline, mailbox, |m| (m.round1.len() as u16 == others).then(|| m.round1.clone()))?;
    let pkgs1: BTreeMap<Identifier, round1::Package> = got1.iter().map(|(i, (pk, _))| (id(*i), pk.clone())).collect();

    // Round 2: one encrypted package per recipient.
    let (s2, out2) = dkg::part2(s1, &pkgs1)?;
    broadcast(p, me, key, deadline, |to| {
        let pkg = out2.get(&id(to)).ok_or_else(|| DkgError::Protocol("missing round-2 package".into()))?;
        let shared = enc_secret.diffie_hellman(&x25519_dalek::PublicKey::from(got1[&to].1)).to_bytes();
        let plain = serde_json::to_vec(pkg).map_err(|e| DkgError::Io(e.to_string()))?;
        // Each key encrypts one message, so a fixed nonce is safe.
        let ciphertext = ChaCha20Poly1305::new(&pair_key(&shared, &context, me, to))
            .encrypt(Nonce::from_slice(&[0; 12]), plain.as_slice())
            .map_err(|_| DkgError::Protocol("encrypt".into()))?;
        Ok(Envelope { context, from: me, body: Body::Round2 { ciphertext } })
    })?;
    let got2 = wait_for("round 2", deadline, mailbox, |m| (m.round2.len() as u16 == others).then(|| m.round2.clone()))?;
    let mut pkgs2: BTreeMap<Identifier, round2::Package> = BTreeMap::new();
    for (from, ct) in &got2 {
        let shared = enc_secret.diffie_hellman(&x25519_dalek::PublicKey::from(got1[from].1)).to_bytes();
        let plain = ChaCha20Poly1305::new(&pair_key(&shared, &context, *from, me))
            .decrypt(Nonce::from_slice(&[0; 12]), ct.as_slice())
            .map_err(|_| DkgError::Protocol(format!("round-2 package from {} does not decrypt", p.participants[*from as usize].name)))?;
        let pkg = serde_json::from_slice(&plain).map_err(|e| DkgError::Protocol(e.to_string()))?;
        pkgs2.insert(id(*from), pkg);
    }
    let (kp, pubkeys) = dkg::part3(&s2, &pkgs1, &pkgs2)?;

    // Round 3: everyone must have seen the same round 1 and derived the same key.
    let mut all1: BTreeMap<u16, round1::Package> = got1.into_iter().map(|(i, (pk, _))| (i, pk)).collect();
    all1.insert(me, pkg1);
    let transcript: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(&all1).map_err(|e| DkgError::Io(e.to_string()))?).into();
    let gk = group_key(&pubkeys);
    broadcast(p, me, key, deadline, |_| Ok(Envelope { context, from: me, body: Body::Confirm { group_key: gk, transcript } }))?;
    let got3 = wait_for("confirmations", deadline, mailbox, |m| (m.confirm.len() as u16 == others).then(|| m.confirm.clone()))?;
    for (from, (their_key, their_transcript)) in got3 {
        if their_key != gk || their_transcript != transcript {
            return Err(DkgError::Protocol(format!(
                "{} derived a different result: an operator sent different round-1 packages; discard everything",
                p.participants[from as usize].name
            )));
        }
    }
    Ok((kp, pubkeys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn setup(ceremonies: [&str; 3]) -> Vec<DkgParams> {
        let seeds: Vec<[u8; 32]> = (0..3).map(|i| [i as u8 + 1; 32]).collect();
        let ports: Vec<u16> = (0..3).map(|_| free_port()).collect();
        let participants: Vec<Participant> = (0..3)
            .map(|i| Participant {
                name: format!("op{i}"),
                url: format!("http://127.0.0.1:{}", ports[i]),
                guardian: SigningKey::from_bytes(&seeds[i]).verifying_key(),
            })
            .collect();
        (0..3)
            .map(|i| DkgParams {
                ceremony: ceremonies[i].into(),
                threshold: 2,
                participants: participants.clone(),
                me: format!("op{i}"),
                guardian_seed: seeds[i],
                listen: format!("127.0.0.1:{}", ports[i]),
                timeout: Duration::from_secs(20),
            })
            .collect()
    }

    fn run_all(ps: Vec<DkgParams>) -> Vec<Result<(KeyPackage, PublicKeyPackage)>> {
        // Start in reverse with a delay: early pushes meet hosts that are not up yet.
        let hs: Vec<_> = ps
            .into_iter()
            .enumerate()
            .rev()
            .map(|(i, p)| {
                std::thread::sleep(Duration::from_millis(300 * i as u64));
                std::thread::spawn(move || run(&p, &mut OsRng))
            })
            .collect();
        let mut out: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        out.reverse();
        out
    }

    #[test]
    fn three_hosts_agree_and_can_sign() {
        let out = run_all(setup(["t1"; 3]));
        let keys: Vec<(KeyPackage, PublicKeyPackage)> = out.into_iter().map(|r| r.unwrap()).collect();
        let gk = group_key(&keys[0].1);
        assert!(keys.iter().all(|(_, pk)| group_key(pk) == gk && *pk == keys[0].1));
        // Any two shares sign under the group key.
        let msg = b"zol";
        let signers = [&keys[0].0, &keys[2].0];
        let rounds: Vec<_> = signers.iter().map(|k| frost_ed25519::round1::commit(k.signing_share(), &mut OsRng)).collect();
        let commitments = signers.iter().zip(&rounds).map(|(k, (_, c))| (*k.identifier(), *c)).collect();
        let pkg = frost_ed25519::SigningPackage::new(commitments, msg);
        let shares = signers
            .iter()
            .zip(&rounds)
            .map(|(k, (nonces, _))| (*k.identifier(), frost_ed25519::round2::sign(&pkg, nonces, k).unwrap()))
            .collect();
        let sig = frost_ed25519::aggregate(&pkg, &shares, &keys[0].1).unwrap();
        let vk = VerifyingKey::from_bytes(&gk).unwrap();
        vk.verify_strict(msg, &ed25519_dalek::Signature::from_slice(&sig.serialize().unwrap()).unwrap()).unwrap();
    }

    #[test]
    fn mismatched_ceremonies_refuse_each_other() {
        let mut ps = setup(["a", "a", "b"]);
        for p in &mut ps {
            p.timeout = Duration::from_secs(5);
        }
        let out = run_all(ps);
        assert!(out.iter().all(|r| r.is_err()));
        assert!(out.iter().any(|r| matches!(r, Err(DkgError::Protocol(m)) if m.contains("different ceremony"))));
    }
}
