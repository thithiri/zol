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

/// Serde helper: byte strings as standard base64 (for large blobs like PCZTs).
pub mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

pub mod api {
    //! `zol-zcashd` HTTP API. All bodies are JSON. Errors are HTTP 4xx/5xx with
    //! `{"error": "..."}`.

    use super::*;

    fn one() -> u32 {
        1
    }

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

    /// `GET /valid_receiver?raw=<hex>`: whether a raw 43-byte receiver is a valid
    /// shielded payment address, and whether it belongs to a vault this daemon
    /// scans (any app, either scope, any diversifier). A refund to a vault
    /// receiver is refused, so the gateway holds such deposits for an operator.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ReceiverCheck {
        pub valid: bool,
        pub vault: bool,
    }

    /// `GET /parse_ua?ua=<unified address>`: the UA's Orchard-protocol receiver on
    /// this daemon's network, with the same checks as `/valid_receiver`. An
    /// operator uses it to turn the address a depositor gives into a refund target.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ParsedUa {
        pub receiver: Vec<u8>,
        pub valid: bool,
        pub vault: bool,
    }

    // ------------------------------------------------------------------
    // Phase 4: the vault head, payouts, and distributed spend signing.

    /// One transition in an app's head chain (design "The vault head").
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct HeadLink {
        /// The head note created by this transition (genesis: the genesis head).
        pub cmx: [u8; 32],
        pub value: u64,
        /// Decoded head memo fields.
        pub position: u64,
        pub paid_count: u64,
        pub records_digest: [u8; 32],
        pub network_fees_spent: u64,
        pub fee_topups_swept: u64,
        /// Transaction that created this head, and its block height.
        pub txid: [u8; 32],
        pub height: u32,
    }

    /// `GET /head?app=<hex>&genesis=<hex>`: the head chain walked by nullifiers
    /// from the genesis commitment pinned on Solana. `chain[0]` is the genesis
    /// head, the last element is the current head. `pending` is a broadcast but
    /// unmined transaction spending the current head, if this daemon knows one.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct HeadChain {
        pub app: AppId,
        pub chain: Vec<HeadLink>,
        pub pending: Option<[u8; 32]>,
        /// Set when the walk is ambiguous (a transition with two vault outputs, a
        /// head memo that does not advance correctly, …). Signers must halt.
        pub fault: Option<String>,
        pub tip: u32,
    }

    /// One payout of a settlement record, with its destination resolved by the
    /// operator's gateway from the deposit memos behind the payout commitment.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PayoutLine {
        pub commitment: [u8; 32],
        pub amount: u64,
        /// 43-byte raw receiver.
        pub to: Vec<u8>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PayoutRecord {
        pub sequence: u64,
        pub payouts: Vec<PayoutLine>,
    }

    /// What one operator's own view says the next head transition must do. Each
    /// operator's gateway builds its own plan from its own finalized Solana reads
    /// and hands it to its own zcashd, which accepts a proposal only if the
    /// proposal does exactly this.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PayoutPlan {
        pub app: AppId,
        /// Genesis head commitment pinned in the `App` account.
        pub genesis_head: [u8; 32],
        /// Consecutive records starting at the current head's `paid_count`.
        /// Empty for a transition that pays nothing (consolidation).
        pub records: Vec<PayoutRecord>,
        /// Deposit notes credited on the finalized ledger (cmx); with full-depth
        /// top-ups, the only notes besides the head a payout may spend.
        pub credited: Vec<[u8; 32]>,
        /// `App.fee_pool` on the finalized ledger.
        pub fee_pool_accrued: u64,
        /// `App.liability` + unpaid settlement amounts not paid by the head
        /// chain and not paid by this transition.
        pub claims_after: u64,
        /// Pad to this many actions (fixed action counts for privacy); 0 = none.
        pub action_target: u32,
        /// At most this many inputs besides the head.
        pub max_inputs: u32,
        /// Depth a top-up must reach before it may be spent (blocks).
        pub full_depth: u32,
        /// Depth the current head must reach before it may be spent. The genesis
        /// head is already at full depth (guardians check it at activation); a
        /// later head was created by the vault itself, and a reorg that removes
        /// it removes its successor too, so 1 is enough.
        #[serde(default = "one")]
        pub head_depth: u32,
        pub expiry_delta: u32,
    }

    /// `POST /payout/propose` (the coordinator's zcashd): build and prove the
    /// next head transition for `plan`. Returns the PCZT for every operator to verify.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Proposal {
        pub app: AppId,
        #[serde(with = "crate::b64")]
        pub pczt: Vec<u8>,
        /// ZIP 244 shielded sighash, which is the txid with no transparent part.
        pub sighash: [u8; 32],
        pub expiry_height: u32,
        pub inputs: Vec<[u8; 32]>,
        pub fee: u64,
        pub new_head_value: u64,
    }

    /// `POST /payout/verify` (every operator's own zcashd): check `pczt` against
    /// this operator's own `plan` and own scan, rule by rule.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct VerifyRequest {
        pub plan: PayoutPlan,
        #[serde(with = "crate::b64")]
        pub pczt: Vec<u8>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct VerifyResponse {
        pub ok: bool,
        /// Every failed check, by name (empty when ok).
        pub failures: Vec<String>,
        pub sighash: [u8; 32],
    }

    /// What is being signed in a distributed spend-signing session.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum SignKind {
        Payout { plan: PayoutPlan },
        /// A refund of one voided deposit note to `to`.
        Refund { app: AppId, cmx: [u8; 32], to: Vec<u8> },
    }

    /// `POST /sign/commit`: FROST round 1 for every vault spend in `pczt`. The
    /// daemon verifies the PCZT against `kind` first (payout: the full rule set
    /// against the operator's own plan; refund: the refund rules) and refuses to
    /// commit otherwise. Nonces are bound to `session` and to the PCZT's sighash.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CommitRequest {
        pub session: u64,
        pub kind: SignKind,
        #[serde(with = "crate::b64")]
        pub pczt: Vec<u8>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CommitResponse {
        /// FROST identifier of this operator's share (serialized).
        #[serde(with = "crate::b64")]
        pub identifier: Vec<u8>,
        /// (action index, serialized SigningCommitments) per vault spend.
        pub commitments: Vec<(u32, Vec<u8>)>,
    }

    /// `POST /sign/share`: FROST round 2. `commitments` are every chosen
    /// signer's round-1 output; the daemon builds the signing packages itself
    /// (message = sighash, randomizer = the action's alpha) and signs.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ShareRequest {
        pub session: u64,
        pub commitments: Vec<CommitResponse>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ShareResponse {
        #[serde(with = "crate::b64")]
        pub identifier: Vec<u8>,
        /// (action index, serialized SignatureShare) per vault spend.
        pub shares: Vec<(u32, Vec<u8>)>,
    }

    /// `POST /sign/aggregate` (the coordinator's zcashd): aggregate, insert the
    /// spend-auth signatures, finalize, extract, verify fully, and broadcast.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct AggregateRequest {
        pub session: u64,
        #[serde(with = "crate::b64")]
        pub pczt: Vec<u8>,
        pub commitments: Vec<CommitResponse>,
        pub shares: Vec<ShareResponse>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Broadcast {
        pub txid: [u8; 32],
        pub expiry_height: u32,
    }

    /// `POST /refund/propose`: build (unsigned, proven) the refund PCZT for one
    /// voided deposit note. Signing then goes through `/sign/*` with
    /// `SignKind::Refund`, so it works with one share per daemon.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RefundProposal {
        #[serde(with = "crate::b64")]
        pub pczt: Vec<u8>,
        pub sighash: [u8; 32],
        pub expiry_height: u32,
        pub value: u64,
        pub fee: u64,
    }
}

pub mod page {
    //! Public HTTP API that `zol-gateway` serves to app pages (browsers). CORS
    //! open, JSON bodies, hex for byte strings in paths and queries. Errors are
    //! 4xx/5xx with `{"error": "..."}`.
    //!
    //! - `GET  /v1/apps/{app}`                         → `AppInfo`
    //! - `GET  /v1/apps/{app}/sessions/{session}`      → `SessionInfo`
    //! - `POST /v1/apps/{app}/intents`  body `Intent`  → `SessionInfo`
    //! - `GET  /v1/apps/{app}/sessions/{session}/deposits` → `Vec<DepositStatus>`
    //!
    //! `{app}` is the app program ID in base58 (Solana form); `{session}` is the
    //! 32-byte session public key in hex.

    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct AppInfo {
        /// App program ID, base58.
        pub app: String,
        /// "regtest" | "testnet" | "mainnet".
        pub network: String,
        /// The address deposit memos sign: raw 43-byte receiver, hex.
        pub vault_address: String,
        /// Permitted deposit amounts in zatoshi.
        pub denominations: Vec<u64>,
        /// Current Zcash tip height seen by this gateway (memo expiry is a height).
        pub tip: u32,
        /// Confirmations for early credit and full depth.
        pub early_depth: u32,
        pub full_depth: u32,
        /// "unregistered" | "pending" | "activating" | "active" | "frozen".
        pub state: String,
        /// Every operator's page API base URL. An intent registered with one is
        /// passed on to the others; a page can also register with each itself.
        #[serde(default)]
        pub operators: Vec<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SessionInfo {
        /// The session's own vault receiver as an Orchard-only unified address,
        /// to put in the ZIP 321 link.
        pub ua: String,
        /// 88-bit diversifier index as a decimal string (JSON numbers lose
        /// precision past 2^53 in browsers).
        pub receiver_index: String,
        /// True once an intent (signed memo) is registered for this session.
        pub intent_registered: bool,
    }

    /// The signed deposit memo, registered before the player pays so a wallet
    /// that drops the memo cannot lose the deposit.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Intent {
        /// Canonical 512-byte memo or its `ZOL1:` text form, hex.
        pub memo: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct DepositStatus {
        /// Note commitment, hex.
        pub cmx: String,
        pub value: u64,
        pub height: u32,
        pub confirmations: u32,
        /// "seen" | "held" | "credited" | "voided" | "refunded"
        pub state: String,
        /// Transaction that paid the deposit, hex in display order (as wallets show it).
        #[serde(default)]
        pub txid: String,
    }
}

