//! Raw block parsing (the node's `getblock <hash|height> 0` returns the serialized block).

use zcash_primitives::{block::Block, transaction::Transaction};

use crate::{params::ChainParams, Error};

pub struct ParsedBlock {
    /// Height claimed by the coinbase (BIP 34).
    pub height: u32,
    /// Block hash, internal byte order (see [`crate::display_hex`]).
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    pub txs: Vec<Transaction>,
}

pub fn parse_block(raw: &[u8], params: &ChainParams) -> Result<ParsedBlock, Error> {
    let b = Block::read(raw, params).map_err(|e| Error::Parse(format!("block: {e}")))?;
    let height = u32::from(b.claimed_height());
    let hash = b.header().hash().0;
    let prev_hash = b.header().prev_block.0;
    let (_, vtx) = b.into_parts();
    Ok(ParsedBlock {
        height,
        hash,
        prev_hash,
        txs: vtx.into(),
    })
}
