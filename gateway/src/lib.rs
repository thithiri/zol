//! ZOL gateway (plan Phase 3): turns what each operator's own Zcash scan shows
//! into threshold-signed credit, void and paid messages for the Solana ledger,
//! reconciles the ledger against real notes, and freezes or halts on mismatch.
//!
//! Zcash-specific work (keys, scanning, transaction building) lives in
//! `zol-zcash`; this crate consumes its output as [`chain::ChainEvent`]s.

pub mod attestor;
pub mod chain;
pub mod config;
pub mod decide;
pub mod ledger;
pub mod policy;
pub mod store;
pub mod node;
pub mod reconcile;
pub mod rpc;
pub mod service;
pub mod zcashd;
