//! Attestation checks: the ledger requires that the instruction just before it is an
//! Ed25519 precompile verification of the attestor key over exactly the expected digest.

use anchor_lang::compat::solana_instructions_sysvar::{load_current_index_checked, load_instruction_at_checked};
use anchor_lang::prelude::*;

use crate::error::ZolError;

pub const INSTRUCTIONS_SYSVAR_ID: Pubkey = pubkey!("Sysvar1nstructions1111111111111111111111111");
pub const ED25519_PROGRAM_ID: Pubkey = pubkey!("Ed25519SigVerify111111111111111111111111111");

/// `sol_sha256` on-chain, software SHA-256 off-chain; both plain SHA-256.
pub struct SolSha256;

impl zol_wire::Hasher for SolSha256 {
    fn hashv(parts: &[&[u8]]) -> [u8; 32] {
        solana_sha256_hasher::hashv(parts).to_bytes()
    }
}

const HEADER: usize = 2; // num_signatures, padding
const OFFSETS: usize = 14; // seven u16 fields
const THIS_IX: u16 = u16::MAX;

fn u16_at(d: &[u8], i: usize) -> Result<u16> {
    d.get(i..i + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| error!(ZolError::BadAttestation))
}

/// Checks the preceding instruction verifies `signer` over `digest` and nothing else.
pub fn require_ed25519(ix_sysvar: &AccountInfo, signer: &Pubkey, digest: &[u8; 32]) -> Result<()> {
    let current = load_current_index_checked(ix_sysvar).map_err(|_| error!(ZolError::BadAttestation))?;
    require!(current > 0, ZolError::BadAttestation);
    let ix = load_instruction_at_checked(current as usize - 1, ix_sysvar)
        .map_err(|_| error!(ZolError::BadAttestation))?;
    require_keys_eq!(ix.program_id, ED25519_PROGRAM_ID, ZolError::BadAttestation);
    let d = &ix.data;
    require!(d.len() >= HEADER + OFFSETS && d[0] == 1, ZolError::BadAttestation);
    let sig_off = u16_at(d, 2)? as usize;
    let sig_ix = u16_at(d, 4)?;
    let pk_off = u16_at(d, 6)? as usize;
    let pk_ix = u16_at(d, 8)?;
    let msg_off = u16_at(d, 10)? as usize;
    let msg_len = u16_at(d, 12)? as usize;
    let msg_ix = u16_at(d, 14)?;
    // All data must live inside the precompile instruction itself, so what was
    // verified is exactly what we read here.
    require!(sig_ix == THIS_IX && pk_ix == THIS_IX && msg_ix == THIS_IX, ZolError::BadAttestation);
    require!(d.get(sig_off..sig_off + 64).is_some(), ZolError::BadAttestation);
    let pk = d.get(pk_off..pk_off + 32).ok_or_else(|| error!(ZolError::BadAttestation))?;
    require!(pk == signer.as_ref(), ZolError::BadAttestation);
    require!(msg_len == 32, ZolError::BadAttestation);
    let msg = d.get(msg_off..msg_off + 32).ok_or_else(|| error!(ZolError::BadAttestation))?;
    require!(msg == digest, ZolError::BadAttestation);
    Ok(())
}
