//! Gateway configuration and per-app gateway secrets (plan 3.1).

use serde::{Deserialize, Serialize};
use zol_wire::Network;

use crate::chain::AppId;

/// Confirmation policy (design v0.3.1, "Confirmation policy"). Depths are in
/// blocks, counted as confirmations (a note in the tip block has 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Depths {
    pub early: u32,
    pub full: u32,
    pub payout: u32,
}

impl Default for Depths {
    fn default() -> Self {
        Self { early: 10, full: 100, payout: 100 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub network: u8,
    pub depths: Depths,
    /// Largest deposit that may be credited before full depth.
    pub early_credit_cap: u64,
    /// Permitted deposit values in zatoshi.
    pub denominations: Vec<u64>,
    /// Seconds without a new tip before the gateway halts.
    pub tip_stall_secs: i64,
}

impl GatewayConfig {
    pub fn network(&self) -> Network {
        Network::from_u8(self.network).expect("valid network")
    }
}

/// Per-app symmetric secrets (design "Keys": "Gateway secrets, fixed per app").
/// Derived from one master secret so a new app needs no new ceremony. They can
/// never be rotated in place: a new deposit-ID key would give one note two IDs.
#[derive(Clone)]
pub struct AppSecrets {
    pub deposit_id_key: [u8; 32],
    pub payout_key: [u8; 32],
}

impl AppSecrets {
    pub fn derive(master: &[u8; 32], network: Network, app: &AppId) -> Self {
        let d = |tag: u8| -> [u8; 32] {
            blake2b_simd::Params::new()
                .hash_length(32)
                .key(master)
                .personal(b"ZOL_GatewaySec_1")
                .to_state()
                .update(&[network as u8, tag])
                .update(app)
                .finalize()
                .as_bytes()
                .try_into()
                .unwrap()
        };
        Self { deposit_id_key: d(0), payout_key: d(1) }
    }

    pub fn deposit_id(&self, cmx: &[u8; 32]) -> [u8; 32] {
        zol_wire::keyed::deposit_id(&self.deposit_id_key, cmx)
    }

    pub fn payout_commitment(&self, addr: &[u8; 43], session: &[u8; 32], app: &AppId) -> [u8; 32] {
        zol_wire::keyed::payout_commitment(&self.payout_key, addr, session, app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_per_app_and_per_network() {
        let m = [1u8; 32];
        let a = AppSecrets::derive(&m, Network::Testnet, &[2; 32]);
        let b = AppSecrets::derive(&m, Network::Testnet, &[3; 32]);
        let c = AppSecrets::derive(&m, Network::Mainnet, &[2; 32]);
        assert_ne!(a.deposit_id_key, b.deposit_id_key);
        assert_ne!(a.deposit_id_key, c.deposit_id_key);
        assert_ne!(a.deposit_id_key, a.payout_key);
    }
}
