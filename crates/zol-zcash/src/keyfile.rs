//! Operator key material on disk: the `[keys]` section `zol-zcashd` loads, trusted-dealer key
//! generation for a fresh operator set (`zol-zcashd keygen`), and the `zol-wallet` funding
//! key file.
//!
//! `generate` is the dev-seed key generation ([`crate::testing::dev_seed_keys`]) with an OS
//! RNG: a [`frost::dealer`] key set (even-Y normalised by the ciphersuite's `post_generate`)
//! and a uniformly random 32-byte master viewing secret. A dealer means one machine saw every
//! share; acceptable for a single-host trial, not for independent operators (they need a
//! Pallas DKG across hosts, [`crate::redpallas::keys::dkg`], which is not wired up yet).

use std::{collections::BTreeMap, path::Path};

use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::{
    frost::{self, Identifier, KeyPackage, KeyPackages, PublicKeyPackage},
    params::{ChainParams, Network},
    Error,
};

/// The `[keys]` config section (hex strings).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeysCfg {
    /// Master viewing secret K, hex (32 bytes). Secret.
    pub master_view: String,
    /// FROST `PublicKeyPackage` (frost-core serialization), hex. `ak_group` is its key.
    pub public_key_package: String,
    /// FROST `KeyPackage`s (frost-core serialization), hex. One in operator mode; at least
    /// the threshold in cluster/dev mode (all shares in one process).
    pub shares: Vec<String>,
}

/// Decoded key material.
pub struct Keys {
    pub master_view: [u8; 32],
    pub shares: BTreeMap<Identifier, KeyPackage>,
    pub pubkeys: PublicKeyPackage,
}

fn hex32(s: &str) -> Result<[u8; 32], String> {
    hex::decode(s.trim())
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "expected 32 bytes of hex".to_string())
}

/// Decodes and checks a `[keys]` section: every share must belong to the public package.
pub fn load(k: &KeysCfg) -> Result<Keys, String> {
    let pubkeys = PublicKeyPackage::deserialize(
        &hex::decode(k.public_key_package.trim()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("public_key_package: {e:?}"))?;
    let mut shares = BTreeMap::new();
    for s in &k.shares {
        let kp = KeyPackage::deserialize(&hex::decode(s.trim()).map_err(|e| e.to_string())?)
            .map_err(|e| format!("share: {e:?}"))?;
        if kp.verifying_key() != pubkeys.verifying_key() {
            return Err("share does not belong to public_key_package".into());
        }
        if pubkeys.verifying_shares().get(kp.identifier()) != Some(kp.verifying_share()) {
            return Err("share's verifying share is not in public_key_package".into());
        }
        shares.insert(*kp.identifier(), kp);
    }
    Ok(Keys {
        master_view: hex32(&k.master_view)?,
        shares,
        pubkeys,
    })
}

/// A file holding only a `[keys]` section (`zol-zcashd keygen` output, `keys_file =`).
#[derive(Deserialize)]
struct KeysFile {
    keys: KeysCfg,
}

pub fn read_keys_file(path: &Path) -> Result<KeysCfg, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str::<KeysFile>(&s)
        .map(|f| f.keys)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A fresh key set: `max_signers` shares, threshold `min_signers`, random master view.
pub fn generate(
    max_signers: u16,
    min_signers: u16,
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<Keys, Error> {
    let (shares, pubkeys) = frost::dealer(max_signers, min_signers, &mut *rng)?;
    let mut master_view = [0u8; 32];
    rng.fill_bytes(&mut master_view);
    Ok(Keys {
        master_view,
        shares,
        pubkeys,
    })
}

/// A public, non-reversible fingerprint of the master viewing secret, so operators can check
/// they hold the same K (which, with `ak_group`, fixes every app's vault address).
pub fn master_view_fingerprint(master_view: &[u8; 32]) -> [u8; 32] {
    blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZOL_mview_fp_v1_")
        .hash(master_view)
        .as_bytes()
        .try_into()
        .unwrap()
}

/// The `[keys]` section holding `shares` (TOML text).
pub fn keys_section(keys: &Keys, shares: &KeyPackages) -> Result<String, Error> {
    let pkp = keys
        .pubkeys
        .serialize()
        .map_err(|e| Error::Frost(format!("{e:?}")))?;
    let shares = shares
        .values()
        .map(|kp| {
            kp.serialize()
                .map(|b| format!("\"{}\"", hex::encode(b)))
                .map_err(|e| Error::Frost(format!("{e:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "[keys]\nmaster_view = \"{}\"\npublic_key_package = \"{}\"\nshares = [\n  {},\n]\n",
        hex::encode(keys.master_view),
        hex::encode(pkp),
        shares.join(",\n  ")
    ))
}

/// The public group description (`group.toml`): no secrets.
pub fn group_toml(keys: &Keys, network: Network) -> Result<String, Error> {
    let pkp = keys
        .pubkeys
        .serialize()
        .map_err(|e| Error::Frost(format!("{e:?}")))?;
    let mut s = format!(
        "# ZOL vault group: public facts only (no secret shares). Every operator must see the\n\
         # same ak_group and master_view_fingerprint (zol-zcashd prints both at startup and in\n\
         # GET /health).\n\
         network = \"{}\"\n\
         operators = {}\n\
         threshold = {}\n\
         ak_group = \"{}\"\n\
         master_view_fingerprint = \"{}\"\n\
         public_key_package = \"{}\"\n\n\
         [verifying_shares]   # FROST identifier (hex) -> verifying share\n",
        network_name(network),
        keys.pubkeys.verifying_shares().len(),
        keys.pubkeys.min_signers().unwrap_or(0),
        hex::encode(frost::group_key(&keys.pubkeys)),
        hex::encode(master_view_fingerprint(&keys.master_view)),
        hex::encode(pkp),
    );
    for (id, vs) in keys.pubkeys.verifying_shares() {
        s.push_str(&format!(
            "\"{}\" = \"{}\"\n",
            hex::encode(id.serialize()),
            hex::encode(vs.serialize().map_err(|e| Error::Frost(format!("{e:?}")))?)
        ));
    }
    Ok(s)
}

pub fn network_name(n: Network) -> &'static str {
    match n {
        Network::Mainnet => "mainnet",
        Network::Testnet => "testnet",
        Network::Regtest => "regtest",
    }
}

pub fn parse_network(s: &str) -> Result<Network, String> {
    match s {
        "mainnet" => Ok(Network::Mainnet),
        "testnet" => Ok(Network::Testnet),
        "regtest" => Ok(Network::Regtest),
        other => Err(format!("unknown network {other}")),
    }
}

/// Creates `path` with `contents`, mode 0600, failing if it already exists (never overwrites
/// key material).
pub fn write_new_secret(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|e| {
        format!(
            "{}: {e} (existing key files are never overwritten)",
            path.display()
        )
    })?;
    f.write_all(contents.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A `zol-wallet` key file: `{"network": "...", "orchard_spending_key": "<hex>"}`.
#[derive(Serialize, Deserialize)]
pub struct WalletFile {
    pub network: String,
    pub orchard_spending_key: String,
}

pub fn read_wallet_file(path: &Path) -> Result<(Network, SpendingKey), String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let w: WalletFile = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    let b = hex32(&w.orchard_spending_key)
        .map_err(|e| format!("{}: orchard_spending_key: {e}", path.display()))?;
    let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes(b))
        .ok_or_else(|| format!("{}: not a valid Orchard spending key", path.display()))?;
    Ok((parse_network(&w.network)?, sk))
}

/// A wallet's Orchard-only unified address (external scope, index 0).
pub fn wallet_ua(params: &ChainParams, sk: &SpendingKey) -> String {
    crate::encode_ua(
        params,
        &FullViewingKey::from(sk)
            .address_at(0u32, Scope::External)
            .to_raw_address_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip_per_operator() {
        let k = generate(3, 2, &mut rand::rngs::OsRng).unwrap();
        // ỹ(ak_group) = 0, as Orchard needs.
        assert_eq!(frost::group_key(&k.pubkeys)[31] >> 7, 0);
        for (id, kp) in &k.shares {
            let one: KeyPackages = [(*id, kp.clone())].into();
            let text = keys_section(&k, &one).unwrap();
            let cfg: KeysFile = toml::from_str(&text).unwrap();
            let back = load(&cfg.keys).unwrap();
            assert_eq!(back.master_view, k.master_view);
            assert_eq!(back.pubkeys, k.pubkeys);
            assert_eq!(back.shares.len(), 1);
            assert_eq!(&back.shares[id], kp);
        }
        let g = group_toml(&k, Network::Testnet).unwrap();
        assert!(!g.contains("shares =") && !g.contains(&hex::encode(k.master_view)));
        let v: toml::Value = toml::from_str(&g).unwrap();
        assert_eq!(v["threshold"].as_integer(), Some(2));
    }

    #[test]
    fn share_from_another_group_is_refused() {
        let a = generate(3, 2, &mut rand::rngs::OsRng).unwrap();
        let b = generate(3, 2, &mut rand::rngs::OsRng).unwrap();
        let mut cfg: KeysFile = toml::from_str(&keys_section(&a, &a.shares).unwrap()).unwrap();
        let other: KeysFile = toml::from_str(&keys_section(&b, &b.shares).unwrap()).unwrap();
        cfg.keys.shares[0] = other.keys.shares[0].clone();
        assert!(load(&cfg.keys).is_err());
    }
}
