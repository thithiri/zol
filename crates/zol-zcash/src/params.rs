//! Consensus parameters for the three networks ZOL runs on.
//!
//! Mainnet and testnet use `zcash_protocol`'s built-in activation heights (NU6.3 /
//! Ironwood at 3,428,143 and 4,134,000). Regtest mirrors `spikes/zebra/regtest.toml`:
//! every upgrade through NU6.3 active from height 1, unless configured otherwise.

use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::{
    BlockHeight, BranchId, NetworkType, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK,
};

pub use zol_wire::Network;

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
}

impl RegtestHeights {
    /// Everything at height 1, as in `spikes/zebra/regtest.toml`.
    pub const ALL_AT_ONE: Self = Self {
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
    };
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
            Network::Mainnet => MAIN_NETWORK.activation_height(nu),
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
        assert_eq!(ChainParams::REGTEST.branch_id(1), BranchId::Nu6_3);
        assert_eq!(ChainParams::TESTNET.branch_id(4_134_000), BranchId::Nu6_3);
        assert_eq!(ChainParams::TESTNET.branch_id(4_133_999), BranchId::Nu6_2);
        assert_eq!(ChainParams::MAINNET.branch_id(3_428_143), BranchId::Nu6_3);
        assert!(ChainParams::REGTEST.ironwood_active(1));
    }
}
