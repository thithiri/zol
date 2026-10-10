//! What the Zcash scanner (`zol-zcashd`) reports to the gateway. The types live
//! in `zol-types`, shared by both processes.

pub use zol_types::{AppId, ChainEvent, ScannedNote, Scope};

/// Fixed-size view of a note's memo.
pub fn memo_array(n: &ScannedNote) -> [u8; zol_wire::MEMO_LEN] {
    let mut m = [0u8; zol_wire::MEMO_LEN];
    let k = n.memo.len().min(zol_wire::MEMO_LEN);
    m[..k].copy_from_slice(&n.memo[..k]);
    m
}

/// Parses a raw receiver as a valid shielded payment address. Provided by
/// `zol-zcash` in production; tests can stub it.
pub trait AddressCheck {
    fn is_valid_receiver(&self, raw: &[u8; zol_wire::ORCHARD_ADDR_LEN]) -> bool;
}

/// Accepts any receiver whose first byte is not 0xFF; for tests only.
pub struct PermissiveAddresses;

impl AddressCheck for PermissiveAddresses {
    fn is_valid_receiver(&self, raw: &[u8; zol_wire::ORCHARD_ADDR_LEN]) -> bool {
        raw[0] != 0xFF
    }
}
