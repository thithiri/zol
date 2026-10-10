//! Types shared by the two halves of a gateway operator:
//!
//! - `zol-zcashd` (workspace `crates/zol-zcash`): Zcash scanner, refund and
//!   genesis builders, Pallas FROST spend shares.
//! - `zol-gateway` (root workspace): decisions, Ed25519 FROST attestation,
//!   Solana relaying, reconciliation, guardians.
//!
//! They run as separate processes because pczt 0.9.3 and anchor-lang 1.2 need
//! incompatible `sha2` releases. They talk JSON over HTTP on localhost; this
//! crate is the whole contract. Byte strings longer than 32 are `Vec<u8>`.

use serde::{Deserialize, Serialize};

pub type AppId = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    External,
    Internal,
}

/// A note that decrypted under one app's vault viewing key (Ironwood pool).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedNote {
    pub app: AppId,
    /// Extracted note commitment, internal byte order.
    pub cmx: [u8; 32],
    pub nullifier: [u8; 32],
    /// Zatoshi.
    pub value: u64,
    /// 512 bytes.
    pub memo: Vec<u8>,
    /// Raw 43-byte receiver the note was sent to.
    pub receiver: Vec<u8>,
    /// Diversifier index of that receiver (88-bit, little-endian in a u128).
    pub receiver_index: u128,
    pub scope: Scope,
    pub height: u32,
    /// Internal byte order.
    pub block_hash: [u8; 32],
    /// Internal byte order.
    pub txid: [u8; 32],
}

/// Events from a scanner, in chain order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainEvent {
    /// A block was connected at this height.
    Block { height: u32, hash: [u8; 32] },
    Note(ScannedNote),
    /// A known note's nullifier was revealed.
    Spent { app: AppId, nullifier: [u8; 32], txid: [u8; 32], height: u32 },
    /// Blocks above `to_height` were disconnected (reorg). Replacement blocks follow.
    Rewind { to_height: u32 },
}

/// Personalization of the session receiver index hash.
pub const SESSION_RECEIVER_PERSONAL: &[u8; 16] = b"ZOL_SessionRecv1";

/// Diversifier index of a session's vault receiver: the first 11 bytes of
/// BLAKE2b-256(personal "ZOL_SessionRecv1", app_id ‖ session_key), read as a
/// little-endian integer. Computable without keys; the receiver itself needs
/// the app's full viewing key (`zol-zcash` `AppKeys::session_receiver`).
pub fn session_receiver_index(app: &AppId, session_key: &[u8; 32]) -> u128 {
    let h = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(SESSION_RECEIVER_PERSONAL)
        .to_state()
        .update(app)
        .update(session_key)
        .finalize();
    let mut b = [0u8; 16];
    b[..11].copy_from_slice(&h.as_bytes()[..11]);
    u128::from_le_bytes(b)
}

pub mod api {
    //! `zol-zcashd` HTTP API. All bodies are JSON. Errors are HTTP 4xx/5xx with
    //! `{"error": "..."}`.

    use super::*;

    /// `GET /events?after=<seq>&limit=<n>`: scanner events with sequence numbers
    /// greater than `after`, in order. `next` is the cursor for the next call.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Events {
        pub events: Vec<(u64, ChainEvent)>,
        pub next: u64,
        /// Scanner tip height (0 before the first block).
        pub tip: u32,
    }

    /// `GET /vault?app=<hex>`
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Vault {
        pub app: AppId,
        /// External receiver, diversifier index 0 (what deposit memos sign).
        pub address: Vec<u8>,
        /// Orchard-only unified address encoding of `address`, for ZIP 321 links.
        pub ua: String,
        pub internal_address: Vec<u8>,
    }

    /// `GET /session_receiver?app=<hex>&session=<hex>`
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SessionReceiver {
        pub receiver: Vec<u8>,
        pub index: u128,
        pub ua: String,
    }

    /// `POST /refund`: spend exactly this deposit note to `to`, less the fee.
    /// Idempotent per note while a broadcast refund is pending.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RefundRequest {
        pub app: AppId,
        pub cmx: [u8; 32],
        /// 43-byte raw receiver from the deposit's memo.
        pub to: Vec<u8>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RefundResponse {
        /// Internal byte order.
        pub txid: [u8; 32],
        pub value: u64,
        pub fee: u64,
        /// Already broadcast earlier for this note.
        pub existing: bool,
    }

    /// `POST /dev/genesis` (only with `--dev`): fund and broadcast a genesis head
    /// for `app` carrying `float`, from the dev wallet.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct GenesisRequest {
        pub app: AppId,
        pub float: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct GenesisResponse {
        pub head_cmx: [u8; 32],
        pub txid: [u8; 32],
        pub head_value: u64,
    }

    /// `POST /dev/send` (only with `--dev`): an ordinary wallet payment from the
    /// dev wallet, as a player's wallet would make it.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SendRequest {
        pub to: Vec<u8>,
        pub value: u64,
        /// 512 bytes (zero padded by the server if shorter).
        pub memo: Vec<u8>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SendResponse {
        pub txid: [u8; 32],
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Error {
        pub error: String,
    }
}
