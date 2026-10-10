//! ZOL's Zcash layer (design v0.3.1; Ironwood pool, NU6.3, v6 transactions).
//!
//! - [`keys`]: per-app vault keys (spike 0.8 spec v1 + ZIP 2005 `use_qsk`), session receivers.
//! - [`frost`]: rerandomized FROST over Pallas with per-app share tweaks.
//! - [`node`]: async JSON-RPC client for the Zcash node (Zakura).
//! - [`block`]: raw block parsing.
//! - [`scan`]: Ironwood (and Orchard) scanner with nullifier tracking, witnesses and reorgs.
//! - [`tx`]: the `pczt` role pipeline and plain Ironwood sends (wallet/test helper).
//! - [`refund`]: deposit refund PCZTs, FROST signing, extraction.
//! - [`genesis`]: the genesis head transaction.
//! - [`verify`]: vault-addressed outputs of a transaction; offline transaction verification.

pub mod block;
pub mod checks;
pub mod frost;
pub mod genesis;
pub mod head;
pub mod keys;
pub mod node;
/// The OS-seeded CSPRNG for orchard, pczt and zcash_primitives (rand_core 0.10).
pub(crate) fn sys_rng() -> rand10::rngs::ThreadRng {
    rand10::rng()
}

pub mod params;
pub mod payout;
pub mod redpallas;
pub mod refund;
pub mod scan;
mod serde_arr;
pub mod testing;
mod tree;
pub mod tx;
pub mod verify;

pub use keys::{encode_ua, AppKeys, Receiver, ReceiverIndex};
pub use orchard::keys::Scope;
pub use params::{ChainParams, Network};
pub use scan::{Account, Pool, ScannedNote, Scanner, SpentNote};

/// ZIP 317 marginal fee and grace actions.
pub const ZIP317_MARGINAL_FEE: u64 = 5_000;
pub const ZIP317_GRACE_ACTIONS: u64 = 2;
/// Upper bound on `expiry_height - target_height` for transactions this crate builds:
/// 300 blocks is about 2 h at NU7's 25 s spacing (ZIP 218).
pub const MAX_EXPIRY_DELTA: u32 = 300;
/// Default expiry delta for refunds, genesis and dev sends: 120 blocks, about 50 min at
/// 25 s spacing (Zcash's 40-block default was set for 75 s blocks). Payouts take theirs
/// from the plan (the gateway's `payout.expiry_delta`, 40 by default, kept short so a
/// reorged-out transition blocks its app for less time).
pub const DEFAULT_EXPIRY_DELTA: u32 = 120;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("key error: {0}")]
    Key(&'static str),
    #[error("frost: {0}")]
    Frost(String),
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("build: {0}")]
    Build(String),
    #[error("pczt: {0}")]
    Pczt(String),
    #[error("verify: {0}")]
    Verify(String),
    #[error("scan: {0}")]
    Scan(String),
    #[error("reorg: block at height {height} does not extend the scanned chain")]
    Reorg { height: u32 },
    #[error("tree: {0}")]
    Tree(String),
}

/// Big-endian hex of a 32-byte hash stored in internal (little-endian) order, as the node's RPC
/// and block explorers display txids and block hashes.
pub fn display_hex(h: &[u8; 32]) -> String {
    let mut r = *h;
    r.reverse();
    hex::encode(r)
}

/// Inverse of [`display_hex`].
pub fn from_display_hex(s: &str) -> Result<[u8; 32], Error> {
    let mut b: [u8; 32] = hex::decode(s)
        .map_err(|e| Error::Parse(e.to_string()))?
        .try_into()
        .map_err(|_| Error::Parse("expected 32 bytes".into()))?;
    b.reverse();
    Ok(b)
}
