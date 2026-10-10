//! Consensus parameters for the three networks ZOL runs on.
//!
//! Mainnet and testnet use `zcash_protocol`'s built-in activation heights (NU6.3 /
//! Ironwood at 3,428,143 and 4,134,000; testnet NU7 at 4,465,026, ZIP 259), except mainnet
//! NU7, which is ours: [`MAINNET_NU7_HEIGHT`] (not yet set; see docs/ops/nu7.md). Regtest defaults to
//! every upgrade through NU7 active from height 1 (`infra/zakura/regtest-nu7.toml`); a
//! pre-NU7 regtest node (`infra/zakura/regtest-pre-nu7.toml`) needs [`RegtestHeights::PRE_NU7`], or
//! [`RegtestHeights::from_upgrades`] to read the heights from the node.

use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::{
    BlockHeight, BranchId, NetworkType, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK,
};

pub use zol_wire::Network;

/// Mainnet NU7 (ZIP 259) activation height. **Set this on 2026-10-20**, when the height is
/// published, as `Some(height)`; it overrides `zcash_protocol`'s mainnet table (which has
/// none in 0.11.0-pre.0) for every builder, parser and verifier in this crate. The gateway
/// has its own `gateway::config::MAINNET_NU7_HEIGHT`; both must change together.
pub const MAINNET_NU7_HEIGHT: Option<u32> = None;

/// Testnet NU7 (ZIP 259). `zcash_protocol` 0.11 already has it; kept here so the two
/// network constants sit side by side (checked against the crate in the tests).
pub const TESTNET_NU7_HEIGHT: u32 = 4_465_026;

/// Mainnet activation heights with NU7 at `nu7` (the rest from `zcash_protocol`).
pub fn mainnet_activation_height(nu: NetworkUpgrade, nu7: Option<u32>) -> Option<BlockHeight> {
    match nu {
        NetworkUpgrade::Nu7 => nu7.map(BlockHeight::from_u32),
        _ => MAIN_NETWORK.activation_height(nu),
    }
}

/// Regtest activation heights (only used when `network == Regtest`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegtestHeights {
    pub overwinter: u32,
    pub sapling: u32,
    pub blossom: u32,
    pub heartwood: u32,
    pub canopy: u32,
    pub nu5: u32,
    pub nu6: u32,
    pub nu6_1: u32,
    pub nu6_2: u32,
    pub nu6_3: u32,
    /// NU7 (ZIP 259). `None`: not active on this regtest chain.
    pub nu7: Option<u32>,
}

impl RegtestHeights {
    /// Everything through NU7 at height 1, as in `infra/zakura/regtest-nu7.toml`.
    pub const ALL_AT_ONE: Self = Self {
        nu7: Some(1),
        ..Self::PRE_NU7
    };

    /// Everything through NU6.3 at height 1 and no NU7, as in
    /// `infra/zakura/regtest-pre-nu7.toml`.
    pub const PRE_NU7: Self = Self {
        overwinter: 1,
        sapling: 1,
        blossom: 1,
        heartwood: 1,
        canopy: 1,
        nu5: 1,
        nu6: 1,
        nu6_1: 1,
        nu6_2: 1,
        nu6_3: 1,
        nu7: None,
    };

    /// Reads the heights from `getblockchaininfo.upgrades` (`{branch: {name,
    /// activationheight}}`, as Zakura reports them). NU5 through NU6.3 are
    /// required (ZOL needs Ironwood); earlier upgrades default to 1; NU7 is optional.
    pub fn from_upgrades(upgrades: &serde_json::Value) -> Result<Self, String> {
        let obj = upgrades
            .as_object()
            .ok_or("getblockchaininfo.upgrades missing")?;
        let get = |name: &str| {
            obj.values()
                .find(|u| u["name"].as_str() == Some(name))
                .and_then(|u| u["activationheight"].as_u64())
                .map(|h| h as u32)
        };
        let need = |name: &str| get(name).ok_or(format!("regtest node does not activate {name}"));
        Ok(Self {
            overwinter: get("Overwinter").unwrap_or(1),
            sapling: get("Sapling").unwrap_or(1),
            blossom: get("Blossom").unwrap_or(1),
            heartwood: get("Heartwood").unwrap_or(1),
            canopy: get("Canopy").unwrap_or(1),
            nu5: need("NU5")?,
            nu6: need("NU6")?,
            nu6_1: need("NU6.1")?,
            nu6_2: need("NU6.2")?,
            nu6_3: need("NU6.3")?,
            nu7: get("NU7"),
        })
    }
}

/// Network plus the consensus parameters needed to parse and build transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainParams {
    #[serde(with = "net_serde")]
    pub network: Network,
    pub regtest: RegtestHeights,
}

mod net_serde {
    use super::Network;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(n: &Network, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(*n as u8)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Network, D::Error> {
        Network::from_u8(u8::deserialize(d)?).ok_or_else(|| serde::de::Error::custom("bad network"))
    }
}

impl ChainParams {
    pub const MAINNET: Self = Self {
        network: Network::Mainnet,
        regtest: RegtestHeights::ALL_AT_ONE,
    };
    pub const TESTNET: Self = Self {
        network: Network::Testnet,
        regtest: RegtestHeights::ALL_AT_ONE,
    };
    pub const REGTEST: Self = Self {
        network: Network::Regtest,
        regtest: RegtestHeights::ALL_AT_ONE,
    };

    pub fn new(network: Network) -> Self {
        Self {
            network,
            regtest: RegtestHeights::ALL_AT_ONE,
        }
    }

    pub fn branch_id(&self, height: u32) -> BranchId {
        BranchId::for_height(self, BlockHeight::from_u32(height))
    }

    /// Regtest with the activation heights a node reports (see
    /// [`RegtestHeights::from_upgrades`]).
    pub fn regtest_from_upgrades(upgrades: &serde_json::Value) -> Result<Self, String> {
        Ok(Self {
            network: Network::Regtest,
            regtest: RegtestHeights::from_upgrades(upgrades)?,
        })
    }

    /// True iff NU7 is active at `height`.
    pub fn nu7_active(&self, height: u32) -> bool {
        self.is_nu_active(NetworkUpgrade::Nu7, BlockHeight::from_u32(height))
    }

    /// True iff NU6.3 (Ironwood) is active at `height`.
    pub fn ironwood_active(&self, height: u32) -> bool {
        self.is_nu_active(NetworkUpgrade::Nu6_3, BlockHeight::from_u32(height))
    }
}

impl Parameters for ChainParams {
    fn network_type(&self) -> NetworkType {
        match self.network {
            Network::Mainnet => NetworkType::Main,
            Network::Testnet => NetworkType::Test,
            Network::Regtest => NetworkType::Regtest,
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self.network {
            Network::Mainnet => mainnet_activation_height(nu, MAINNET_NU7_HEIGHT),
            Network::Testnet => TEST_NETWORK.activation_height(nu),
            Network::Regtest => {
                let r = &self.regtest;
                let h = match nu {
                    NetworkUpgrade::Overwinter => r.overwinter,
                    NetworkUpgrade::Sapling => r.sapling,
                    NetworkUpgrade::Blossom => r.blossom,
                    NetworkUpgrade::Heartwood => r.heartwood,
                    NetworkUpgrade::Canopy => r.canopy,
                    NetworkUpgrade::Nu5 => r.nu5,
                    NetworkUpgrade::Nu6 => r.nu6,
                    NetworkUpgrade::Nu6_1 => r.nu6_1,
                    NetworkUpgrade::Nu6_2 => r.nu6_2,
                    NetworkUpgrade::Nu6_3 => r.nu6_3,
                    NetworkUpgrade::Nu7 => r.nu7?,
                    #[allow(unreachable_patterns)]
                    _ => return None,
                };
                Some(BlockHeight::from_u32(h))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches() {
        assert_eq!(ChainParams::REGTEST.branch_id(1), BranchId::Nu7);
        assert_eq!(u32::from(BranchId::Nu7), 0x7719_0ad9);
        assert_eq!(BranchId::try_from(0x7719_0ad9), Ok(BranchId::Nu7));
        let pre = ChainParams {
            network: Network::Regtest,
            regtest: RegtestHeights::PRE_NU7,
        };
        assert_eq!(pre.branch_id(1), BranchId::Nu6_3);
        assert_eq!(ChainParams::TESTNET.branch_id(4_465_025), BranchId::Nu6_3);
        assert_eq!(ChainParams::TESTNET.branch_id(4_465_026), BranchId::Nu7);
        match MAINNET_NU7_HEIGHT {
            None => {
                assert!(!ChainParams::MAINNET.nu7_active(10_000_000));
                assert_eq!(ChainParams::MAINNET.branch_id(10_000_000), BranchId::Nu6_3);
            }
            Some(h) => {
                assert_eq!(ChainParams::MAINNET.branch_id(h - 1), BranchId::Nu6_3);
                assert_eq!(ChainParams::MAINNET.branch_id(h), BranchId::Nu7);
            }
        }
        assert_eq!(
            TEST_NETWORK.activation_height(NetworkUpgrade::Nu7),
            Some(BlockHeight::from_u32(TESTNET_NU7_HEIGHT))
        );
        assert_eq!(ChainParams::TESTNET.branch_id(4_134_000), BranchId::Nu6_3);
        assert_eq!(ChainParams::TESTNET.branch_id(4_133_999), BranchId::Nu6_2);
        assert_eq!(ChainParams::MAINNET.branch_id(3_428_143), BranchId::Nu6_3);
        assert!(ChainParams::REGTEST.ironwood_active(1));
    }

    /// What setting [`MAINNET_NU7_HEIGHT`] does, without setting it: a `LocalNetwork` with
    /// mainnet's heights and NU7 at `n` agrees with [`mainnet_activation_height`] for every
    /// upgrade, switches branch exactly at `n`, and caps expiry before it.
    #[test]
    fn mainnet_nu7_transition() {
        use zcash_protocol::local_consensus::LocalNetwork;
        let n = 3_600_000u32; // hypothetical; must be a multiple of 3 (ZIP 259)
        let h = |nu| mainnet_activation_height(nu, Some(n));
        let local = LocalNetwork {
            overwinter: h(NetworkUpgrade::Overwinter),
            sapling: h(NetworkUpgrade::Sapling),
            blossom: h(NetworkUpgrade::Blossom),
            heartwood: h(NetworkUpgrade::Heartwood),
            canopy: h(NetworkUpgrade::Canopy),
            nu5: h(NetworkUpgrade::Nu5),
            nu6: h(NetworkUpgrade::Nu6),
            nu6_1: h(NetworkUpgrade::Nu6_1),
            nu6_2: h(NetworkUpgrade::Nu6_2),
            nu6_3: h(NetworkUpgrade::Nu6_3),
            nu7: h(NetworkUpgrade::Nu7),
        };
        assert_eq!(local.nu6_3, Some(BlockHeight::from_u32(3_428_143)));
        let at = |x: u32| BranchId::for_height(&local, BlockHeight::from_u32(x));
        assert_eq!(at(n - 1), BranchId::Nu6_3);
        assert_eq!(at(n), BranchId::Nu7);
        assert_eq!(u32::from(at(n)), 0x7719_0ad9);
        // A transaction targeted before the upgrade never outlives it.
        assert_eq!(crate::tx::capped_expiry(&local, n - 10, 40), n - 1);
        assert_eq!(crate::tx::capped_expiry(&local, n, 40), n + 40);
        // Unset, the mainnet table has no NU7.
        assert_eq!(mainnet_activation_height(NetworkUpgrade::Nu7, None), None);
    }
}
