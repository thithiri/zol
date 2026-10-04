//! ZOL Phase 0 spikes 0.3-0.8: Orchard/Ironwood vault payouts through the real `pczt`
//! roles, the signer-side recompute checks, and rerandomized FROST over Pallas.
//!
//! Everything here is verified offline: we build our own note commitment tree, take
//! the anchor and paths from it, and verify the extracted transaction locally
//! (Halo2 proof, spend-auth signatures and binding signature against the ZIP 244
//! sighash recomputed from the parsed raw transaction).

pub mod checks;
pub mod frost_vault;
pub mod inspect;
pub mod notes;
pub mod payout;
pub mod perapp;
pub mod pool;
pub mod tree;
pub mod verify;

/// Writes `data` (hex) into `spikes/zcash/out/<name>` so the broadcast stub can pick it up.
pub fn write_out(name: &str, data: &[u8]) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("out");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(name), hex::encode(data));
}
