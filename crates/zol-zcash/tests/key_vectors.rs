//! Key-derivation cross-checks and frozen vectors (docs/review/key-derivation.md).
//!
//! 1. `zip2005_matches_third_party_vectors`: our ZIP 2005 `use_qsk = true` derivation
//!    reproduces Zafe's vectors (`tests/vectors/zip2005_use_qsk_zafe.json`, MIT/Apache-2.0,
//!    not official; also proposed upstream as ZcashFoundation/frost-tools#610).
//! 2. `independent_rederivation`: a second implementation of the whole ZOL derivation,
//!    written from the spec in this file without calling `zol_zcash::keys`, checked
//!    against the frozen vectors.
//! 3. `frozen_vectors`: `AppKeys` reproduces `tests/vectors/zol_keys_v1.json` exactly.
//!    `ZOL_REGEN_VECTORS=1` rewrites the file (only on an intentional, versioned change).
//! 4. `python_verifier`: `tests/vectors/verify_zol_keys.py` (Python standard library only,
//!    its own BLAKE3 and Pallas arithmetic) re-derives the same vectors.

use ff::{FromUniformBytes, PrimeField};
use group::{Group, GroupEncoding};
use orchard::keys::{FullViewingKey, Scope};
use pasta_curves::pallas;
use serde_json::{json, Value};
use zcash_address::unified::{self, Encoding};
use zol_wire::Network;
use zol_zcash::{keys, AppKeys};

fn h(s: &Value) -> Vec<u8> {
    hex::decode(s.as_str().unwrap()).unwrap()
}
fn h32(s: &Value) -> [u8; 32] {
    h(s).try_into().unwrap()
}

#[test]
fn zip2005_matches_third_party_vectors() {
    let f: Value = serde_json::from_str(include_str!("vectors/zip2005_use_qsk_zafe.json")).unwrap();
    assert_eq!(f["qk_context"], "Zcash ZIP 2005 qk-derivation v1");
    let vs = f["vectors"].as_array().unwrap();
    for v in vs {
        let (sk, ak) = (h32(&v["sk"]), h32(&v["ak"]));
        let z = keys::zip2005_use_qsk(&sk, &ak);
        assert_eq!(hex::encode(z.nk), v["nk"].as_str().unwrap());
        assert_eq!(hex::encode(z.qsk), v["qsk"].as_str().unwrap());
        assert_eq!(hex::encode(z.qk), v["qk"].as_str().unwrap());
        assert_eq!(hex::encode(z.rivk_ext), v["rivk_ext"].as_str().unwrap());
        let fvk_bytes: [u8; 96] = [ak.as_slice(), &z.nk, &z.rivk_ext]
            .concat()
            .try_into()
            .unwrap();
        assert_eq!(hex::encode(fvk_bytes), v["fvk"].as_str().unwrap());
        let fvk = FullViewingKey::from_bytes(&fvk_bytes).expect("valid FVK");
        assert_eq!(
            hex::encode(fvk.address_at(0u32, Scope::External).to_raw_address_bytes()),
            v["default_address_raw"].as_str().unwrap()
        );
        let ufvk = unified::Ufvk::try_from_items(
            unified::Revision::R0,
            vec![unified::Uitem::Data(unified::Fvk::Orchard(fvk_bytes))],
        )
        .unwrap();
        assert_eq!(
            ufvk.encode(&zcash_protocol::consensus::NetworkType::Test),
            v["ufvk_testnet"].as_str().unwrap()
        );
    }
    println!("ZIP 2005 use_qsk: {} third-party vectors reproduced (nk, qsk, qk, rivk_ext, FVK, address, UFVK)", vs.len());
}

// ---------------------------------------------------------------- independent re-derivation

fn blake(personal: &[u8; 16], len: usize, parts: &[&[u8]]) -> Vec<u8> {
    let mut st = blake2b_simd::Params::new()
        .hash_length(len)
        .personal(personal)
        .to_state();
    for p in parts {
        st.update(p);
    }
    st.finalize().as_bytes().to_vec()
}
fn expand(key: &[u8], t: &[&[u8]]) -> [u8; 64] {
    blake(b"Zcash_ExpandSeed", 64, &[&[key], t].concat())
        .try_into()
        .unwrap()
}
fn to_scalar(b: &[u8]) -> pallas::Scalar {
    pallas::Scalar::from_uniform_bytes(&b.try_into().unwrap())
}
fn to_base(b: &[u8]) -> pallas::Base {
    pallas::Base::from_uniform_bytes(&b.try_into().unwrap())
}

/// ZOL v1 from the spec text (docs/review/key-derivation.md § 3), not from `zol_zcash::keys`.
fn rederive(ak_group: [u8; 32], k: [u8; 32], app: [u8; 32], net: u8) -> Value {
    let g = pallas::Point::from_bytes(&h32(&json!(GENERATOR_HEX))).unwrap();
    let akg = pallas::Point::from_bytes(&ak_group).unwrap();
    let d = [1u8, net];
    let mut enc = 32u16.to_le_bytes().to_vec();
    enc.extend_from_slice(&app);
    let mut rejected = vec![];
    for c in 0u32.. {
        let cb = c.to_le_bytes();
        let t = to_scalar(&blake(b"ZOL_app_ak_tweak", 64, &[&d, &ak_group, &enc, &cb]));
        let p = akg + g * t;
        let akb = p.to_bytes();
        if bool::from(p.is_identity()) || akb[31] >> 7 == 1 {
            rejected.push(json!({"c": c, "tweak": hex::encode(t.to_repr()), "ak_candidate": hex::encode(akb)}));
            continue;
        }
        let sk: [u8; 32] = blake(b"ZOL_app_view_key", 32, &[&d, &k, &enc, &cb])
            .try_into()
            .unwrap();
        let nk = to_base(&expand(&sk, &[&[0x07]]));
        let qsk: [u8; 32] = expand(&sk, &[&[0x0C]])[..32].try_into().unwrap();
        let qk = blake3::derive_key("Zcash ZIP 2005 qk-derivation v1", &qsk);
        let rivk = to_scalar(&expand(&qk, &[&[0x0D], &akb, &nk.to_repr()]));
        // ZIP 32 internal rivk, and dk/ovk for both scopes (§ 4.2.3 / ZIP 32), recomputed here.
        let rivk_int = to_scalar(&expand(&rivk.to_repr(), &[&[0x83], &akb, &nk.to_repr()]));
        let ovk = |r: &pallas::Scalar| {
            hex::encode(&expand(&r.to_repr(), &[&[0x82], &akb, &nk.to_repr()])[32..])
        };
        let fvk: Vec<u8> = [akb.as_slice(), &nk.to_repr(), &rivk.to_repr()].concat();
        let pad = blake(b"ZOL_padding_ovk1", 32, &[&fvk]);
        return json!({
            "counter": c, "rejected": rejected, "tweak": hex::encode(t.to_repr()), "ak_app": hex::encode(akb),
            "view_secret": hex::encode(sk), "nk": hex::encode(nk.to_repr()), "qsk": hex::encode(qsk), "qk": hex::encode(qk),
            "rivk_ext": hex::encode(rivk.to_repr()), "rivk_internal": hex::encode(rivk_int.to_repr()), "fvk": hex::encode(&fvk),
            "ovk_external": ovk(&rivk), "ovk_internal": ovk(&rivk_int), "padding_ovk": hex::encode(pad),
        });
    }
    unreachable!()
}

// ---------------------------------------------------------------- frozen vectors

const GENERATOR_HEX: &str = "63c975b884721a8d0ca1707be30c7f0c5f445f3e7c188d3b06d6f128b32355b7";
const NETS: [(Network, &str); 3] = [
    (Network::Mainnet, "mainnet"),
    (Network::Testnet, "testnet"),
    (Network::Regtest, "regtest"),
];

/// ak_group = [s]G for a fixed s, negated if ỹ = 1 (as reddsa normalizes a FROST key).
fn group_key(s: u64) -> [u8; 32] {
    let mut p = keys::spend_auth_generator() * pallas::Scalar::from(s);
    if p.to_bytes()[31] >> 7 == 1 {
        p = -p;
    }
    p.to_bytes()
}

fn generate() -> Value {
    assert_eq!(
        hex::encode(keys::spend_auth_generator().to_bytes()),
        GENERATOR_HEX
    );
    let ak_group = group_key(0x5a5a_5a5a);
    let k = [0x11u8; 32];
    // Two fixed app IDs, plus the first [i; 32] that needs a counter retry on mainnet.
    let retry = (1u8..=255)
        .map(|i| [i; 32])
        .find(|a| {
            AppKeys::derive(ak_group, &k, a, Network::Mainnet)
                .unwrap()
                .counter
                > 0
        })
        .unwrap();
    let apps = [[0xA1u8; 32], [0xB2; 32], retry];
    let sessions = [[0u8; 32], [0x5E; 32]];
    let mut cases = vec![];
    for (net, name) in NETS {
        for app in apps {
            let a = AppKeys::derive(ak_group, &k, &app, net).unwrap();
            let z = keys::zip2005_use_qsk(&a.view_secret(), &a.ak);
            let fvk = a.fvk();
            let rivk_int = fvk.to_ivk(Scope::Internal); // only to force internal derivation
            let _ = rivk_int;
            let mut rejected = vec![];
            for c in 0..a.counter {
                let t = keys::app_tweak(&ak_group, &app, net, c);
                let p = pallas::Point::from_bytes(&ak_group).unwrap()
                    + keys::spend_auth_generator() * t;
                rejected.push(json!({"c": c, "tweak": hex::encode(t.to_repr()), "ak_candidate": hex::encode(p.to_bytes())}));
            }
            cases.push(json!({
                "network": name,
                "network_byte": net as u8,
                "app_id": hex::encode(app),
                "counter": a.counter,
                "rejected": rejected,
                "tweak": hex::encode(a.tweak),
                "ak_app": hex::encode(a.ak),
                "view_secret": hex::encode(a.view_secret()),
                "nk": hex::encode(z.nk),
                "qsk": hex::encode(z.qsk),
                "qk": hex::encode(z.qk),
                "rivk_ext": hex::encode(z.rivk_ext),
                "fvk": hex::encode(fvk.to_bytes()),
                "ovk_external": hex::encode(a.ovk(Scope::External).as_ref()),
                "ovk_internal": hex::encode(a.ovk(Scope::Internal).as_ref()),
                "padding_ovk": hex::encode(a.padding_ovk().as_ref()),
                "vault_address": hex::encode(a.vault_address()),
                "vault_internal_address": hex::encode(a.vault_internal_address()),
                "sessions": sessions.iter().map(|s| {
                    let (r, idx) = a.session_receiver(s);
                    json!({"session_key": hex::encode(s), "index": hex::encode(idx), "receiver": hex::encode(r)})
                }).collect::<Vec<_>>(),
            }));
        }
    }
    json!({
        "description": "ZOL per-app vault key derivation, KEY_DERIVATION_VERSION 1 (docs/review/key-derivation.md). All values hex; scalars and base-field elements are little-endian repr; points are repr_P.",
        "key_derivation_version": keys::KEY_DERIVATION_VERSION,
        "spend_auth_generator": GENERATOR_HEX,
        "ak_group_scalar": "0x5a5a5a5a (negated if needed so that ỹ(ak_group) = 0)",
        "ak_group": hex::encode(ak_group),
        "master_view": hex::encode(k),
        "cases": cases,
    })
}

const VECTORS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/vectors/zol_keys_v1.json"
);

#[test]
fn frozen_vectors() {
    let now = generate();
    if std::env::var("ZOL_REGEN_VECTORS").is_ok() {
        std::fs::write(VECTORS, serde_json::to_string_pretty(&now).unwrap() + "\n").unwrap();
        println!("wrote {VECTORS}");
        return;
    }
    let frozen: Value = serde_json::from_str(
        &std::fs::read_to_string(VECTORS).expect("vectors file (ZOL_REGEN_VECTORS=1 to create)"),
    )
    .unwrap();
    assert_eq!(
        frozen["key_derivation_version"],
        now["key_derivation_version"]
    );
    for (f, n) in frozen["cases"]
        .as_array()
        .unwrap()
        .iter()
        .zip(now["cases"].as_array().unwrap())
    {
        for (key, val) in f.as_object().unwrap() {
            assert_eq!(
                &n[key], val,
                "{} {} field {key} changed",
                f["network"], f["app_id"]
            );
        }
    }
    assert_eq!(frozen, now, "vectors changed");
    let cases = frozen["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    assert!(
        cases.iter().any(|c| c["counter"].as_u64().unwrap() > 0),
        "a counter-retry case is included"
    );
}

#[test]
fn independent_rederivation() {
    let f: Value = serde_json::from_str(&std::fs::read_to_string(VECTORS).unwrap()).unwrap();
    let ak_group = h32(&f["ak_group"]);
    let k = h32(&f["master_view"]);
    for c in f["cases"].as_array().unwrap() {
        let net = c["network_byte"].as_u64().unwrap() as u8;
        let r = rederive(ak_group, k, h32(&c["app_id"]), net);
        for key in [
            "counter",
            "rejected",
            "tweak",
            "ak_app",
            "view_secret",
            "nk",
            "qsk",
            "qk",
            "rivk_ext",
            "fvk",
            "ovk_external",
            "ovk_internal",
            "padding_ovk",
        ] {
            assert_eq!(r[key], c[key], "{} {} {key}", c["network"], c["app_id"]);
        }
        // Session index: BLAKE2b-256("ZOL_SessionRecv1", app_id || session_key)[..11].
        for s in c["sessions"].as_array().unwrap() {
            let idx = &blake(
                b"ZOL_SessionRecv1",
                32,
                &[&h(&c["app_id"]), &h(&s["session_key"])],
            )[..11];
            assert_eq!(hex::encode(idx), s["index"].as_str().unwrap());
        }
    }
}

#[test]
fn python_verifier() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/verify_zol_keys.py"
    );
    let zafe = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/zip2005_use_qsk_zafe.json"
    );
    match std::process::Command::new("python3")
        .args(["-I", script, VECTORS, zafe])
        .output()
    {
        Ok(o) => {
            print!("{}", String::from_utf8_lossy(&o.stdout));
            assert!(
                o.status.success(),
                "python verifier failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        Err(e) => println!("python3 not available ({e}); skipped"),
    }
}

/// The generator used for the tweak is Orchard's spend-authorization base point: an ordinary
/// Orchard key's `ak` equals `[ask]G` (with the spec's ỹ = 0 negation).
#[test]
fn generator_is_orchard_spendauth_base() {
    let g = pallas::Point::from_bytes(&h32(&json!(GENERATOR_HEX))).unwrap();
    for i in 0u8..8 {
        let Some(sk) = Option::<orchard::keys::SpendingKey>::from(
            orchard::keys::SpendingKey::from_bytes([i; 32]),
        ) else {
            continue;
        };
        let ask = to_scalar(&expand(sk.to_bytes(), &[&[0x06]]));
        let mut ak = g * ask;
        if ak.to_bytes()[31] >> 7 == 1 {
            ak = -ak;
        }
        assert_eq!(ak.to_bytes(), FullViewingKey::from(&sk).to_bytes()[..32]);
    }
}
