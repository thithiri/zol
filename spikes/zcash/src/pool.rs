//! Which Orchard-protocol value pool / network upgrade a spike targets.
//!
//! The pinned `zcash_protocol` 0.10.6 knows NU6.3 ("Ironwood"): from NU6.3 the Orchard
//! value pool requires `enableCrossAddress = 0` (a spend can only pay its own address)
//! and ordinary payments move to the Ironwood pool (v6 transactions, V3 note
//! plaintexts). We therefore run the payout in both shapes:
//! - `OrchardNu62`: v5 transaction, Orchard pool, protocol V2 (the design's assumption);
//! - `IronwoodNu63`: v6 transaction, Ironwood pool, protocol V3.

use std::sync::OnceLock;

use orchard::{
    bundle::BundleVersion,
    circuit::{OrchardCircuitVersion, ProvingKey, VerifyingKey},
    ValuePool,
};
use zcash_protocol::consensus::{BlockHeight, BranchId, TEST_NETWORK};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pool {
    /// Testnet height in [NU6.2, NU6.3): v5 tx, Orchard pool, cross-address allowed.
    OrchardNu62,
    /// Testnet height >= NU6.3: v6 tx, Ironwood pool, cross-address allowed.
    IronwoodNu63,
}

impl Pool {
    pub fn bundle_version(self) -> BundleVersion {
        match self {
            Pool::OrchardNu62 => BundleVersion::orchard_v2(),
            Pool::IronwoodNu63 => BundleVersion::ironwood_v3(),
        }
    }

    pub fn value_pool(self) -> ValuePool {
        self.bundle_version().value_pool()
    }

    /// A testnet target height inside the upgrade's epoch (TEST_NETWORK parameters from
    /// `zcash_protocol` 0.10.6: NU6.2 = 4_052_000, NU6.3 = 4_134_000).
    pub fn target_height(self) -> BlockHeight {
        match self {
            Pool::OrchardNu62 => BlockHeight::from_u32(4_100_000),
            Pool::IronwoodNu63 => BlockHeight::from_u32(4_200_000),
        }
    }

    pub fn branch_id(self) -> BranchId {
        BranchId::for_height(&TEST_NETWORK, self.target_height())
    }

    pub fn circuit_version(self) -> OrchardCircuitVersion {
        self.bundle_version().circuit_version()
    }

    pub fn proving_key(self) -> &'static ProvingKey {
        static PK_V2: OnceLock<ProvingKey> = OnceLock::new();
        static PK_V3: OnceLock<ProvingKey> = OnceLock::new();
        let cell = match self {
            Pool::OrchardNu62 => &PK_V2,
            Pool::IronwoodNu63 => &PK_V3,
        };
        cell.get_or_init(|| ProvingKey::build(self.circuit_version()))
    }

    pub fn verifying_key(self) -> &'static VerifyingKey {
        static VK_V2: OnceLock<VerifyingKey> = OnceLock::new();
        static VK_V3: OnceLock<VerifyingKey> = OnceLock::new();
        let cell = match self {
            Pool::OrchardNu62 => &VK_V2,
            Pool::IronwoodNu63 => &VK_V3,
        };
        cell.get_or_init(|| VerifyingKey::build(self.circuit_version()))
    }

    pub const ALL: [Pool; 2] = [Pool::OrchardNu62, Pool::IronwoodNu63];
}
