//! Confirmation policy and float accounting (plan 3.3; design "Confirmation
//! policy", "Reserve accounting").

use crate::config::Depths;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    Unconfirmed,
    Early,
    Full,
}

/// Confirmations of a note mined at `height` when the tip is `tip` (the tip
/// block itself is one confirmation), in blocks.
pub fn confirmations(height: u32, tip: u32) -> u32 {
    if tip < height {
        0
    } else {
        tip - height + 1
    }
}

/// Depths measured in time rather than blocks. `depths` are in 25 s blocks;
/// a block below `nu7_height` is a 75 s block and counts three times. So the
/// policy switches by itself at NU7 activation, and a confirmation count never
/// drops as the tip crosses it (a note at full depth stays at full depth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthRule {
    pub depths: Depths,
    pub nu7_height: u32,
}

/// 75 s / 25 s.
const PRE_NU7_WEIGHT: u32 = 3;

impl DepthRule {
    /// Every block counts once (regtest, unit tests).
    pub fn flat(depths: Depths) -> Self {
        Self { depths, nu7_height: 0 }
    }

    /// Confirmations of a block at `height` with tip `tip`, in 25 s blocks.
    pub fn confirmations(&self, height: u32, tip: u32) -> u32 {
        if tip < height {
            return 0;
        }
        let fast = confirmations(height.max(self.nu7_height), tip);
        let slow = tip.saturating_add(1).min(self.nu7_height).saturating_sub(height);
        fast.saturating_add(slow.saturating_mul(PRE_NU7_WEIGHT))
    }

    /// Fewest blocks, counting back from the tip, that make `units`
    /// confirmations: what a node that counts plain blocks should wait for.
    pub fn blocks(&self, units: u32, tip: u32) -> u32 {
        let fast = if tip >= self.nu7_height { tip - self.nu7_height + 1 } else { 0 };
        if units <= fast {
            units
        } else {
            fast + (units - fast).div_ceil(PRE_NU7_WEIGHT)
        }
    }

    pub fn depth(&self, height: u32, tip: u32) -> Depth {
        let c = self.confirmations(height, tip);
        if c >= self.depths.full {
            Depth::Full
        } else if c >= self.depths.early {
            Depth::Early
        } else {
            Depth::Unconfirmed
        }
    }
}

pub fn depth(height: u32, tip: u32, r: &DepthRule) -> Depth {
    r.depth(height, tip)
}

/// An app's reserve position, as one signer computes it from its own scan and
/// the finalized ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reserve {
    /// Head value + credited deposit notes (early or full) + full-depth top-ups, unspent.
    pub reserve: u128,
    /// App.liability + unpaid settlements.
    pub claims: u128,
    /// Fee pool still held in ZEC: accrued on Solana + fee top-ups swept − network fees spent.
    pub fee_pool_left: u128,
}

impl Reserve {
    /// What is left of the float after claims and the fee pool; zero if the
    /// float has been exhausted (a loss beyond it is a global halt).
    pub fn remaining_float(&self) -> u128 {
        self.reserve.saturating_sub(self.claims).saturating_sub(self.fee_pool_left)
    }

    /// True when claims plus fee pool exceed the reserve.
    pub fn is_short(&self) -> bool {
        self.claims + self.fee_pool_left > self.reserve
    }
}

/// May a deposit of `amount` be credited before full depth?
/// `pending_early` is the value of early credits not yet at full depth.
pub fn early_credit_allowed(amount: u64, pending_early: u128, per_deposit_cap: u64, r: &Reserve) -> bool {
    amount <= per_deposit_cap && pending_early + amount as u128 <= r.remaining_float()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_classes() {
        let d = DepthRule::flat(Depths { early: 10, full: 100, payout: 100 });
        assert_eq!(depth(100, 108, &d), Depth::Unconfirmed);
        assert_eq!(depth(100, 109, &d), Depth::Early);
        assert_eq!(depth(100, 199, &d), Depth::Full);
        assert_eq!(depth(100, 50, &d), Depth::Unconfirmed);
    }

    /// Before NU7 the 25 s-block depths need a third of the blocks; after, all
    /// of them; across activation, a mix. Confirmations never drop as the tip
    /// advances, so nothing falls back from full depth at the switch.
    #[test]
    fn depths_switch_at_nu7() {
        let n = 1_000;
        let r = DepthRule { depths: Depths::NU7, nu7_height: n };
        // Entirely before NU7: 10 blocks of 75 s.
        assert_eq!(r.depth(500, 508), Depth::Unconfirmed);
        assert_eq!(r.depth(500, 509), Depth::Early);
        assert_eq!(r.depth(500, 599), Depth::Full);
        // Entirely after: 30 blocks of 25 s.
        assert_eq!(r.depth(2_000, 2_028), Depth::Unconfirmed);
        assert_eq!(r.depth(2_000, 2_029), Depth::Early);
        assert_eq!(r.depth(2_000, 2_299), Depth::Full);
        // Mined 5 blocks before activation (15 units), so 15 more after.
        assert_eq!(r.confirmations(995, 999), 15);
        assert_eq!(r.depth(995, 1_013), Depth::Unconfirmed);
        assert_eq!(r.depth(995, 1_014), Depth::Early);
        // `blocks` inverts `confirmations` at the tip.
        for tip in [900, 999, 1_000, 1_010, 1_400] {
            for units in [1, 2, 3, 30, 31, 300] {
                let k = r.blocks(units, tip);
                assert!(r.confirmations(tip + 1 - k, tip) >= units, "tip {tip} units {units}");
                assert!(r.confirmations(tip + 2 - k, tip) < units, "tip {tip} units {units} minimal");
            }
        }
        // Monotone across the boundary.
        let mut last = 0;
        for tip in 990..1_020 {
            let c = r.confirmations(980, tip);
            assert!(c > last);
            last = c;
        }
        // Mainnet before the height is announced: every block is a slow one.
        let pre = DepthRule { depths: Depths::NU7, nu7_height: u32::MAX };
        assert_eq!(pre.blocks(300, 3_000_000), 100);
        assert_eq!(pre.depth(3_000_000, 3_000_099), Depth::Full);
    }

    /// The worked example in design v0.3.1, "Reserve accounting".
    #[test]
    fn design_float_example() {
        // Float 100,000; Alice 1,000,000 at full depth; one early credit of 100,000; 0.3% entry fee.
        let r = Reserve { reserve: 1_200_000, claims: 997_000 + 99_700, fee_pool_left: 3_000 + 300 };
        assert_eq!(r.remaining_float(), 100_000);
        assert!(early_credit_allowed(0, 100_000, 1_000_000, &r), "the early credit just fits");
        assert!(!early_credit_allowed(1, 100_000, 1_000_000, &r));
        // The early credit is rolled back: the float absorbs the loss, entry fee included.
        let after = Reserve { reserve: 1_100_000, ..r };
        assert_eq!(after.remaining_float(), 0);
        assert!(!after.is_short());
        assert!(!early_credit_allowed(1, 0, 1_000_000, &after));
    }
}
