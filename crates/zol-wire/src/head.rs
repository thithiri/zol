//! Head memo and top-up memo (design v0.3, "The vault head", "Reserve accounting").
//!
//! Head memo v1:
//!
//! | Bytes   | Field                                         |
//! |---------|-----------------------------------------------|
//! | 0–4     | 0xFF "ZOL" 0x10                               |
//! | 5       | network                                       |
//! | 6–37    | app program ID                                |
//! | 38–45   | position in the head chain (LE), genesis = 0  |
//! | 46–53   | settlements paid so far (LE); next seq = this |
//! | 54–85   | records digest of this transition (0 if none) |
//! | 86–93   | running total: network fees spent (LE)        |
//! | 94–101  | running total: fee top-ups swept in (LE)      |
//! | 102–511 | zero                                          |
//!
//! Top-up memo v1:
//!
//! | Bytes  | Field                          |
//! |--------|--------------------------------|
//! | 0–4    | 0xFF "ZOL" 0x11                |
//! | 5      | network                        |
//! | 6–37   | app program ID                 |
//! | 38     | purpose: 0 float, 1 fee pool   |
//! | 39–511 | zero                           |

use crate::memo::{all_zero, classify, header, MemoError, MemoKind, KIND_HEAD_V1, KIND_TOPUP_V1};
use crate::{Network, MEMO_LEN};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadMemo {
    pub network: Network,
    pub app: [u8; 32],
    pub position: u64,
    pub paid_count: u64,
    pub records_digest: [u8; 32],
    pub network_fees_spent: u64,
    pub fee_topups_swept: u64,
}

/// What one head transition does, used to check that the new head memo advances
/// the old one by exactly that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub records_paid: u64,
    pub records_digest: [u8; 32],
    pub network_fee: u64,
    pub fee_topups_swept: u64,
}

impl HeadMemo {
    pub fn genesis(network: Network, app: [u8; 32]) -> Self {
        Self {
            network,
            app,
            position: 0,
            paid_count: 0,
            records_digest: [0; 32],
            network_fees_spent: 0,
            fee_topups_swept: 0,
        }
    }

    pub fn is_genesis(&self) -> bool {
        *self == Self::genesis(self.network, self.app)
    }

    /// The memo the next head must carry after `t`. `None` on overflow.
    pub fn advance(&self, t: &Transition) -> Option<Self> {
        if t.records_paid == 0 && t.records_digest != [0; 32] {
            return None;
        }
        Some(Self {
            network: self.network,
            app: self.app,
            position: self.position.checked_add(1)?,
            paid_count: self.paid_count.checked_add(t.records_paid)?,
            records_digest: t.records_digest,
            network_fees_spent: self.network_fees_spent.checked_add(t.network_fee)?,
            fee_topups_swept: self.fee_topups_swept.checked_add(t.fee_topups_swept)?,
        })
    }

    pub fn encode(&self) -> [u8; MEMO_LEN] {
        let mut m = [0u8; MEMO_LEN];
        m[..5].copy_from_slice(&header(KIND_HEAD_V1));
        m[5] = self.network as u8;
        m[6..38].copy_from_slice(&self.app);
        m[38..46].copy_from_slice(&self.position.to_le_bytes());
        m[46..54].copy_from_slice(&self.paid_count.to_le_bytes());
        m[54..86].copy_from_slice(&self.records_digest);
        m[86..94].copy_from_slice(&self.network_fees_spent.to_le_bytes());
        m[94..102].copy_from_slice(&self.fee_topups_swept.to_le_bytes());
        m
    }

    pub fn decode(m: &[u8; MEMO_LEN]) -> Result<Self, MemoError> {
        if classify(m)? != MemoKind::HeadV1 {
            return Err(MemoError::UnknownKind(m[4]));
        }
        if !all_zero(&m[102..]) {
            return Err(MemoError::NonZeroPadding);
        }
        let u = |a: usize| u64::from_le_bytes(m[a..a + 8].try_into().unwrap());
        Ok(Self {
            network: Network::from_u8(m[5]).ok_or(MemoError::BadNetwork)?,
            app: m[6..38].try_into().unwrap(),
            position: u(38),
            paid_count: u(46),
            records_digest: m[54..86].try_into().unwrap(),
            network_fees_spent: u(86),
            fee_topups_swept: u(94),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TopUpPurpose {
    Float = 0,
    FeePool = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopUpMemo {
    pub network: Network,
    pub app: [u8; 32],
    pub purpose: TopUpPurpose,
}

impl TopUpMemo {
    pub fn encode(&self) -> [u8; MEMO_LEN] {
        let mut m = [0u8; MEMO_LEN];
        m[..5].copy_from_slice(&header(KIND_TOPUP_V1));
        m[5] = self.network as u8;
        m[6..38].copy_from_slice(&self.app);
        m[38] = self.purpose as u8;
        m
    }

    pub fn decode(m: &[u8; MEMO_LEN]) -> Result<Self, MemoError> {
        if classify(m)? != MemoKind::TopUpV1 {
            return Err(MemoError::UnknownKind(m[4]));
        }
        if !all_zero(&m[39..]) {
            return Err(MemoError::NonZeroPadding);
        }
        Ok(Self {
            network: Network::from_u8(m[5]).ok_or(MemoError::BadNetwork)?,
            app: m[6..38].try_into().unwrap(),
            purpose: match m[38] {
                0 => TopUpPurpose::Float,
                1 => TopUpPurpose::FeePool,
                _ => return Err(MemoError::BadPurpose),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn genesis_and_advance() {
        let g = HeadMemo::genesis(Network::Testnet, [7; 32]);
        assert!(g.is_genesis());
        assert_eq!(HeadMemo::decode(&g.encode()).unwrap(), g);
        let t = Transition { records_paid: 2, records_digest: [1; 32], network_fee: 15_000, fee_topups_swept: 0 };
        let n = g.advance(&t).unwrap();
        assert_eq!((n.position, n.paid_count, n.network_fees_spent), (1, 2, 15_000));
        assert!(!n.is_genesis());
        // A transition paying nothing must not carry a records digest.
        let bad = Transition { records_paid: 0, ..t };
        assert!(g.advance(&bad).is_none());
    }

    #[test]
    fn head_rejects_padding_and_kind() {
        let g = HeadMemo::genesis(Network::Mainnet, [7; 32]).encode();
        let mut m = g;
        m[200] = 1;
        assert_eq!(HeadMemo::decode(&m), Err(MemoError::NonZeroPadding));
        let mut m = g;
        m[5] = 9;
        assert_eq!(HeadMemo::decode(&m), Err(MemoError::BadNetwork));
        let t = TopUpMemo { network: Network::Mainnet, app: [7; 32], purpose: TopUpPurpose::FeePool };
        assert!(HeadMemo::decode(&t.encode()).is_err());
        assert_eq!(TopUpMemo::decode(&t.encode()).unwrap(), t);
        let mut m = t.encode();
        m[38] = 2;
        assert_eq!(TopUpMemo::decode(&m), Err(MemoError::BadPurpose));
    }

    proptest! {
        #[test]
        fn head_roundtrip(app in any::<[u8;32]>(), pos in any::<u64>(), paid in any::<u64>(), d in any::<[u8;32]>(), f in any::<u64>(), s in any::<u64>()) {
            let h = HeadMemo { network: Network::Regtest, app, position: pos, paid_count: paid, records_digest: d, network_fees_spent: f, fee_topups_swept: s };
            prop_assert_eq!(HeadMemo::decode(&h.encode()).unwrap(), h);
        }
    }
}
