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

impl Depths {
    /// Before NU7 (75 s blocks): 10 / 100 / 100, about 12 min / 2 h / 2 h.
    pub const PRE_NU7: Depths = Depths { early: 10, full: 100, payout: 100 };
    /// From NU7 (25 s blocks, ZIP 218): the same times, three times the blocks.
    /// Still far inside the node's 1,000-block reorg window.
    pub const NU7: Depths = Depths { early: 30, full: 300, payout: 300 };
}

impl Default for Depths {
    /// NU7 is active on testnet and scheduled for mainnet; a pre-NU7 mainnet
    /// deployment should configure `Depths::PRE_NU7` explicitly.
    fn default() -> Self {
        Self::NU7
    }
}

/// Payout policy (design signer rule 8, "Privacy": schedule and padding).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutPolicy {
    /// Most settlement records paid in one head transition.
    pub max_records_per_tx: u32,
    /// Most inputs besides the head.
    pub max_inputs: u32,
    /// Fixed action counts; a transition is padded up to the smallest that fits.
    pub action_sizes: Vec<u32>,
    /// Pay at most this often per app (seconds), unless `batch_records` are waiting.
    pub interval_secs: i64,
    pub batch_records: u32,
    /// A payout line above this needs every operator and `large_delay_secs`.
    pub payout_cap: u64,
    pub large_delay_secs: i64,
    /// Total paid per app per rolling hour.
    pub hourly_cap: u64,
    /// Transitions that pay nothing (consolidation): at most this often.
    pub consolidate_interval_secs: i64,
    /// Consolidate when this many spendable non-head notes have accumulated.
    pub consolidate_at_notes: u32,
    pub expiry_delta: u32,
}

impl Default for PayoutPolicy {
    fn default() -> Self {
        Self {
            max_records_per_tx: 4,
            max_inputs: 8,
            action_sizes: vec![4, 8, 16, 32],
            interval_secs: 0,
            batch_records: 1,
            payout_cap: 100_000_000_000,
            large_delay_secs: 3_600,
            hourly_cap: u64::MAX,
            consolidate_interval_secs: 3_600,
            consolidate_at_notes: 16,
            expiry_delta: 40,
        }
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
    #[serde(default)]
    pub payout: PayoutPolicy,
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
    fn nu7_depths_keep_pre_nu7_wall_clock_times() {
        // ZIP 218: 75 s -> 25 s blocks, so the same times need 3x the blocks.
        let (a, b) = (Depths::PRE_NU7, Depths::NU7);
        assert_eq!((b.early, b.full, b.payout), (3 * a.early, 3 * a.full, 3 * a.payout));
        assert_eq!(Depths::default(), Depths::NU7);
        assert!(b.full < 1_000, "inside the node's reorg window");
    }

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
