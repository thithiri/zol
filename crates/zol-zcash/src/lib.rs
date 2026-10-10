//! ZOL's Zcash layer (design v0.3.1; Ironwood pool, NU6.3, v6 transactions).
//!
//! - [`keys`]: per-app vault keys (spike 0.8 spec v1 + ZIP 2005 `use_qsk`), session receivers.
//! - [`frost`]: rerandomized FROST over Pallas with per-app share tweaks.
//! - [`zebra`]: async JSON-RPC client for Zebra 6.4.2.
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
pub mod keys;
pub mod params;
pub mod refund;
pub mod scan;
mod serde_arr;
pub mod testing;
mod tree;
pub mod tx;
pub mod verify;
pub mod zebra;

pub use keys::{encode_ua, AppKeys, Receiver, ReceiverIndex};
pub use orchard::keys::Scope;
pub use params::{ChainParams, Network};
pub use scan::{Account, Pool, ScannedNote, Scanner, SpentNote};

/// ZIP 317 marginal fee and grace actions.
pub const ZIP317_MARGINAL_FEE: u64 = 5_000;
pub const ZIP317_GRACE_ACTIONS: u64 = 2;
/// Upper bound on `expiry_height - target_height` for transactions this crate builds.
pub const MAX_EXPIRY_DELTA: u32 = 100;
/// Default expiry delta (Zcash's default, `DEFAULT_TX_EXPIRY_DELTA`).
pub const DEFAULT_EXPIRY_DELTA: u32 = 40;

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

/// Big-endian hex of a 32-byte hash stored in internal (little-endian) order, as Zebra's RPC
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
