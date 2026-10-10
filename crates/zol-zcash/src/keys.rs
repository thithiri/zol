//! Per-app vault keys (spec: `docs/spikes/0.8-per-app-key-derivation.md`, production
//! version 1, with the network and a version byte in every derivation input, and the
//! viewing components derived per ZIP 2005 with `use_qsk = true`).
//!
//! ```text
//! D        = [KEY_DERIVATION_VERSION] || [network]
//! enc(app) = I2LEOSP_16(32) || app_id
//! for c = 0, 1, 2, ...:
//!   t_c    = ToScalar(BLAKE2b-512("ZOL_app_ak_tweak", D || ak_group || enc(app) || I2LEOSP_32(c)))
//!   A_c    = ak_group + [t_c] G_spendauth          reject if 𝒪 or ỹ(A_c) = 1
//!   sk_c   = BLAKE2b-256("ZOL_app_view_key", D || K || enc(app) || I2LEOSP_32(c))
//!   -- ZIP 2005 § 4.2.3, use_qsk = true, with ak^P = A_c ("Usage with FROST"):
//!   nk     = H^nk(sk_c)   = ToBase  (PRF^expand_{sk_c}([0x07]))
//!   qsk    = H^qsk(sk_c)  = truncate_32(PRF^expand_{sk_c}([0x0C]))
//!   qk     = H^qk(qsk)    = BLAKE3.derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)
//!   rivk   = H^rivk_ext_qk(ak, nk) = ToScalar(PRF^expand_qk([0x0D] || I2LEOSP_256(ak) || I2LEOSP_256(nk)))
//!   reject if FullViewingKey::from_bytes(ak || nk || rivk) fails
//! ```
//!
//! Internal-scope keys follow ZIP 32 unchanged (`rivk_int = PRF^expand_rivk([0x83] || ak || nk)`,
//! which ZIP 2005 keeps), and are computed by `orchard`.
//!
//! **ZIP 326 / ZIP 2005.** The vault keys are `use_qsk = true` keys: ZIP 2005 ("Usage with
//! FROST") says FROST participants MUST derive `nk`, `qsk`, `qk` and `rivk_ext` this way, and
//! ZIP 326 allows generating such keys after NU6.3 Mainnet activation, which has happened.
//! Senders are unaffected: a payment needs only the 43-byte receiver `(d, pk_d)`, and the
//! sender-side encryption never touches `rivk` (ZIP 326, "Receiving funds into the Ironwood
//! pool"). The regtest integration test pays a `use_qsk = true` session receiver from an
//! ordinary `use_qsk = false` wallet key and the scanner finds the note. Because `use_qsk` is
//! fixed per vault, the scanner uses exactly one `ivk` per scope; it never needs ZIP 326's
//! four-way first-funds search. Operators must retain `K` (from which every `qsk` can be
//! rederived) and `ak_group` for the future Recovery Protocol.

use blake2b_simd::Params as Blake2b;
use ff::{FromUniformBytes, PrimeField};
use group::{Group, GroupEncoding};
use orchard::keys::{
    DiversifierIndex, FullViewingKey, IncomingViewingKey, OutgoingViewingKey, Scope,
};
use pasta_curves::pallas;
use zcash_address::unified::{self, Container, Encoding};

use crate::params::{ChainParams, Network};
use crate::Error;

/// Version of the production derivation. Spike 0.8's vectors are "version 0" (no D prefix,
/// ad-hoc nk/rivk) and are not compatible.
pub const KEY_DERIVATION_VERSION: u8 = 1;
pub const TWEAK_PERSONAL: &[u8; 16] = b"ZOL_app_ak_tweak";
pub const VIEW_PERSONAL: &[u8; 16] = b"ZOL_app_view_key";
pub const SESSION_PERSONAL: &[u8; 16] = b"ZOL_SessionRecv1";
pub const PADDING_OVK_PERSONAL: &[u8; 16] = b"ZOL_padding_ovk1";
const ZIP2005_QK_CONTEXT: &str = "Zcash ZIP 2005 qk-derivation v1";

/// A raw Orchard-protocol receiver (diversifier 11 bytes || pk_d 32 bytes).
pub type Receiver = [u8; 43];
/// A ZIP 32 diversifier index, little-endian 11 bytes.
pub type ReceiverIndex = [u8; 11];

pub(crate) fn prf_expand(sk: &[u8], t: &[&[u8]]) -> [u8; 64] {
    let mut st = Blake2b::new()
        .hash_length(64)
        .personal(b"Zcash_ExpandSeed")
        .to_state();
    st.update(sk);
    for p in t {
        st.update(p);
    }
    *st.finalize().as_array()
}

fn blake(personal: &[u8; 16], len: usize, parts: &[&[u8]]) -> Vec<u8> {
    let mut st = Blake2b::new()
        .hash_length(len)
        .personal(personal)
        .to_state();
    for p in parts {
        st.update(p);
    }
    st.finalize().as_bytes().to_vec()
}

pub(crate) fn to_scalar(h: &[u8; 64]) -> pallas::Scalar {
    pallas::Scalar::from_uniform_bytes(h)
}
pub(crate) fn to_base(h: &[u8; 64]) -> pallas::Base {
    pallas::Base::from_uniform_bytes(h)
}

/// G^Orchard, the spend-authorization base point (also FROST(Pallas)'s generator).
pub fn spend_auth_generator() -> pallas::Point {
    use group::GroupEncoding;
    // reddsa's `orchard::SpendAuth` basepoint encoding (protocol spec 5.4.7.1), as reddsa
    // 0.5.2's FROST ciphersuite used it.
    const SPEND_AUTH_BASEPOINT: [u8; 32] = [
        99, 201, 117, 184, 132, 114, 26, 141, 12, 161, 112, 123, 227, 12, 127, 12, 95, 68, 95, 62,
        124, 24, 141, 59, 6, 214, 241, 40, 179, 35, 85, 183,
    ];
    pallas::Point::from_bytes(&SPEND_AUTH_BASEPOINT).unwrap()
}

fn app_enc(app_id: &[u8; 32]) -> [u8; 34] {
    let mut v = [0u8; 34];
    v[..2].copy_from_slice(&32u16.to_le_bytes());
    v[2..].copy_from_slice(app_id);
    v
}

/// ZIP 2005 § 4.2.3 (`use_qsk = true`) components, as bytes: `nk` (I2LEOSP_256 of the
/// base-field element), `qsk`, `qk` and `rivk_ext` (I2LEOSP_256 of the scalar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zip2005Keys {
    pub nk: [u8; 32],
    pub qsk: [u8; 32],
    pub qk: [u8; 32],
    pub rivk_ext: [u8; 32],
}

/// ZIP 2005 § 4.2.3 with `use_qsk = true`, from the agreed secret `sk` and the FROST `ak`
/// (32-byte `repr_P` with ỹ = 0, which equals `I2LEOSP_256(Extract_P(ak^P))`).
pub fn zip2005_use_qsk(sk: &[u8; 32], ak: &[u8; 32]) -> Zip2005Keys {
    let (nk, qsk, qk, rivk) = zip2005_qsk_components(sk, ak);
    Zip2005Keys {
        nk: nk.to_repr(),
        qsk,
        qk,
        rivk_ext: rivk.to_repr(),
    }
}

/// The ZIP 2005 `use_qsk = true` viewing components for a given `ak` and per-app `sk`.
pub(crate) fn zip2005_qsk_components(
    sk: &[u8; 32],
    ak: &[u8; 32],
) -> (pallas::Base, [u8; 32], [u8; 32], pallas::Scalar) {
    let nk = to_base(&prf_expand(sk, &[&[0x07]]));
    let qsk: [u8; 32] = prf_expand(sk, &[&[0x0C]])[..32].try_into().unwrap();
    let qk = blake3::derive_key(ZIP2005_QK_CONTEXT, &qsk);
    let rivk = to_scalar(&prf_expand(&qk, &[&[0x0D], ak, &nk.to_repr()]));
    (nk, qsk, qk, rivk)
}

/// The public tweak candidate `t_c` for counter `c` (see the module docs).
pub fn app_tweak(
    ak_group: &[u8; 32],
    app_id: &[u8; 32],
    network: Network,
    c: u32,
) -> pallas::Scalar {
    let d = [KEY_DERIVATION_VERSION, network as u8];
    let enc = app_enc(app_id);
    to_scalar(
        &blake(TWEAK_PERSONAL, 64, &[&d, ak_group, &enc, &c.to_le_bytes()])
            .try_into()
            .unwrap(),
    )
}

/// The keys of one app's vault.
#[derive(Clone, Debug)]
pub struct AppKeys {
    pub app_id: [u8; 32],
    pub network: Network,
    /// Accepted derivation counter `c`.
    pub counter: u32,
    /// The public spend-key tweak `t_app`, little-endian scalar bytes. Every signer adds it to
    /// its base FROST share.
    pub tweak: [u8; 32],
    /// `ak_app = ak_group + [t_app]G`, ỹ = 0.
    pub ak: [u8; 32],
    sk: [u8; 32],
    qsk: [u8; 32],
    fvk: FullViewingKey,
}

impl AppKeys {
    /// Derives an app's vault keys from the FROST group key, the master viewing secret `K`
    /// and the app's Solana program ID.
    pub fn derive(
        ak_group: [u8; 32],
        master_view: &[u8; 32],
        app_id: &[u8; 32],
        network: Network,
    ) -> Result<Self, Error> {
        let ak_g = Option::<pallas::Point>::from(pallas::Point::from_bytes(&ak_group))
            .filter(|p| !bool::from(p.is_identity()))
            .ok_or(Error::Key(
                "ak_group is not a valid non-identity Pallas point",
            ))?;
        let d = [KEY_DERIVATION_VERSION, network as u8];
        let enc = app_enc(app_id);
        for c in 0u32..=u32::MAX {
            let cb = c.to_le_bytes();
            let t = to_scalar(
                &blake(TWEAK_PERSONAL, 64, &[&d, &ak_group, &enc, &cb])
                    .try_into()
                    .unwrap(),
            );
            let p = ak_g + spend_auth_generator() * t;
            if bool::from(p.is_identity()) {
                continue;
            }
            let ak = p.to_bytes();
            if ak[31] >> 7 == 1 {
                continue;
            }
            let sk: [u8; 32] = blake(VIEW_PERSONAL, 32, &[&d, master_view, &enc, &cb])
                .try_into()
                .unwrap();
            let (nk, qsk, _qk, rivk) = zip2005_qsk_components(&sk, &ak);
            let mut b = [0u8; 96];
            b[..32].copy_from_slice(&ak);
            b[32..64].copy_from_slice(&nk.to_repr());
            b[64..].copy_from_slice(&rivk.to_repr());
            if let Some(fvk) = FullViewingKey::from_bytes(&b) {
                return Ok(Self {
                    app_id: *app_id,
                    network,
                    counter: c,
                    tweak: t.to_repr(),
                    ak,
                    sk,
                    qsk,
                    fvk,
                });
            }
        }
        Err(Error::Key("no valid counter"))
    }

    pub fn fvk(&self) -> &FullViewingKey {
        &self.fvk
    }

    pub fn tweak_scalar(&self) -> pallas::Scalar {
        pallas::Scalar::from_repr(self.tweak).unwrap()
    }

    /// ZIP 2005 quantum spending key. Must be retained (rederivable from `K`).
    pub fn qsk(&self) -> [u8; 32] {
        self.qsk
    }

    /// ZIP 2005 quantum intermediate key `qk = H^qk(qsk)`.
    pub fn qk(&self) -> [u8; 32] {
        blake3::derive_key(ZIP2005_QK_CONTEXT, &self.qsk)
    }

    /// The per-app 32-byte secret `sk_c` the viewing components come from (ZIP 2005 `sk`).
    pub fn view_secret(&self) -> [u8; 32] {
        self.sk
    }

    pub fn ivk(&self, scope: Scope) -> IncomingViewingKey {
        self.fvk.to_ivk(scope)
    }

    pub fn ovk(&self, scope: Scope) -> OutgoingViewingKey {
        self.fvk.to_ovk(scope)
    }

    /// The vault-derived OVK for padding outputs: BLAKE2b-256("ZOL_padding_ovk1", fvk bytes).
    /// Anyone with the FVK (every signer) can recompute it.
    pub fn padding_ovk(&self) -> OutgoingViewingKey {
        let h: [u8; 32] = blake(PADDING_OVK_PERSONAL, 32, &[&self.fvk.to_bytes()])
            .try_into()
            .unwrap();
        OutgoingViewingKey::from(h)
    }

    /// The vault's public receiver: external scope, diversifier index 0.
    pub fn vault_address(&self) -> Receiver {
        self.fvk
            .address_at(0u32, Scope::External)
            .to_raw_address_bytes()
    }

    /// The head's receiver: internal scope, index 0.
    pub fn vault_internal_address(&self) -> Receiver {
        self.fvk
            .address_at(0u32, Scope::Internal)
            .to_raw_address_bytes()
    }

    /// Deterministic per-session diversified external receiver:
    /// index = first 11 bytes of BLAKE2b-256("ZOL_SessionRecv1", app_id || session_key).
    pub fn session_receiver(&self, session_key: &[u8; 32]) -> (Receiver, ReceiverIndex) {
        let h = blake(SESSION_PERSONAL, 32, &[&self.app_id, session_key]);
        let idx: [u8; 11] = h[..11].try_into().unwrap();
        let addr = self
            .fvk
            .address_at(DiversifierIndex::from(idx), Scope::External);
        (addr.to_raw_address_bytes(), idx)
    }

    /// If `addr` belongs to this vault (either scope, any diversifier), its index and scope.
    pub fn receiver_index(&self, addr: &Receiver) -> Option<(ReceiverIndex, Scope)> {
        let a = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(addr))?;
        let scope = self.fvk.scope_for_address(&a)?;
        let idx = self.fvk.to_ivk(scope).diversifier_index(&a)?;
        Some((*idx.as_bytes(), scope))
    }
}

/// Encodes a raw Orchard-protocol receiver as a Unified Address containing only that
/// receiver (the form wallets accept in ZIP 321 links; Ironwood uses the same receivers).
pub fn encode_ua(params: &ChainParams, receiver: &Receiver) -> String {
    use zcash_protocol::consensus::Parameters;
    // Revision 0 (ZIP 316 `u1`/`utest1`/`uregtest1`), the form every wallet reads.
    let ua = unified::Address::try_from_items(
        unified::Revision::R0,
        vec![unified::Uitem::Data(unified::Receiver::Orchard(*receiver))],
    )
    .expect("one receiver");
    ua.encode(&params.network_type())
}

/// Decodes a Unified Address and returns its Orchard-protocol receiver, if any.
pub fn decode_ua_orchard(params: &ChainParams, ua: &str) -> Option<Receiver> {
    use zcash_protocol::consensus::Parameters;
    // Either revision (R0 `u1…` or R2 `zu1…`); only the Orchard receiver matters.
    let (net, _revision, addr) = unified::Address::decode(ua).ok()?;
    if net != params.network_type() {
        return None;
    }
    addr.items().into_iter().find_map(|r| match r {
        unified::Receiver::Orchard(b) => Some(b),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group_key(seed: u64) -> [u8; 32] {
        let mut p = spend_auth_generator() * pallas::Scalar::from(seed);
        if p.to_bytes()[31] >> 7 == 1 {
            p = -p;
        }
        p.to_bytes()
    }

    #[test]
    fn derive_basic_properties() {
        let ak_group = group_key(0x5a5a_5a5a);
        let k = [0x11u8; 32];
        let a = AppKeys::derive(ak_group, &k, &[0xA1; 32], Network::Regtest).unwrap();
        let b = AppKeys::derive(ak_group, &k, &[0xB2; 32], Network::Regtest).unwrap();
        let a_test = AppKeys::derive(ak_group, &k, &[0xA1; 32], Network::Testnet).unwrap();
        assert_eq!(a.ak[31] >> 7, 0);
        assert_ne!(a.ak, b.ak);
        assert_ne!(a.ak, a_test.ak, "network bound");
        assert_ne!(a.fvk.to_bytes(), a_test.fvk.to_bytes());
        // ak_app = ak_group + [t]G
        let g = Option::<pallas::Point>::from(pallas::Point::from_bytes(&ak_group)).unwrap();
        assert_eq!(
            (g + spend_auth_generator() * a.tweak_scalar()).to_bytes(),
            a.ak
        );
        // Deterministic.
        let a2 = AppKeys::derive(ak_group, &k, &[0xA1; 32], Network::Regtest).unwrap();
        assert_eq!(a2.fvk.to_bytes(), a.fvk.to_bytes());
        assert_eq!(a2.counter, a.counter);
        // FVK = (ak, nk, rivk) where rivk follows ZIP 2005 use_qsk = true.
        let (nk, _qsk, qk, rivk) = zip2005_qsk_components(&a.view_secret(), &a.ak);
        assert_eq!(&a.fvk.to_bytes()[32..64], &nk.to_repr());
        assert_eq!(&a.fvk.to_bytes()[64..], &rivk.to_repr());
        assert_eq!(qk, a.qk());
    }

    #[test]
    fn counter_retry_happens_and_is_sound() {
        let ak_group = group_key(7);
        let mut retried = 0;
        for i in 0..64u8 {
            let k = AppKeys::derive(ak_group, &[3; 32], &[i; 32], Network::Mainnet).unwrap();
            assert_eq!(k.ak[31] >> 7, 0);
            retried += (k.counter > 0) as u32;
        }
        assert!(
            retried > 10 && retried < 54,
            "about half need a retry: {retried}"
        );
    }

    #[test]
    fn receivers() {
        let a = AppKeys::derive(group_key(9), &[1; 32], &[0xA1; 32], Network::Regtest).unwrap();
        let b = AppKeys::derive(group_key(9), &[1; 32], &[0xB2; 32], Network::Regtest).unwrap();
        assert_eq!(
            a.receiver_index(&a.vault_address()),
            Some(([0; 11], Scope::External))
        );
        assert_eq!(
            a.receiver_index(&a.vault_internal_address()),
            Some(([0; 11], Scope::Internal))
        );
        let (r1, i1) = a.session_receiver(&[5; 32]);
        let (r2, i2) = a.session_receiver(&[6; 32]);
        assert_ne!(r1, r2);
        assert_eq!(a.session_receiver(&[5; 32]), (r1, i1), "deterministic");
        assert_eq!(a.receiver_index(&r1), Some((i1, Scope::External)));
        assert_eq!(a.receiver_index(&r2), Some((i2, Scope::External)));
        assert_eq!(b.receiver_index(&r1), None, "other app does not own it");
        assert_ne!(a.padding_ovk().as_ref(), a.ovk(Scope::External).as_ref());
        let ua = encode_ua(&ChainParams::REGTEST, &r1);
        assert!(ua.starts_with("uregtest1"));
        assert_eq!(decode_ua_orchard(&ChainParams::REGTEST, &ua), Some(r1));
        assert_eq!(decode_ua_orchard(&ChainParams::MAINNET, &ua), None);
    }

    #[test]
    fn rejects_bad_group_key() {
        assert!(AppKeys::derive([0; 32], &[1; 32], &[1; 32], Network::Regtest).is_err());
    }

    /// Prints deterministic vectors for docs/phase3-zol-zcash.md.
    #[test]
    fn vectors() {
        let ak_group = group_key(0x5a5a_5a5a);
        for (net, app) in [
            (Network::Mainnet, [0xA1u8; 32]),
            (Network::Testnet, [0xA1; 32]),
            (Network::Regtest, [0xB2; 32]),
        ] {
            let k = AppKeys::derive(ak_group, &[0x11; 32], &app, net).unwrap();
            println!(
                "net={net:?} app={} c={} t={} ak={} fvk={} qk={} vault_addr={}",
                hex::encode(&app[..2]),
                k.counter,
                hex::encode(k.tweak),
                hex::encode(k.ak),
                hex::encode(k.fvk.to_bytes()),
                hex::encode(k.qk()),
                hex::encode(k.vault_address())
            );
        }
    }
}
