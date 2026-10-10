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
/// block itself is one confirmation).
pub fn confirmations(height: u32, tip: u32) -> u32 {
    if tip < height {
        0
    } else {
        tip - height + 1
    }
}

pub fn depth(height: u32, tip: u32, d: &Depths) -> Depth {
    let c = confirmations(height, tip);
    if c >= d.full {
        Depth::Full
    } else if c >= d.early {
        Depth::Early
    } else {
        Depth::Unconfirmed
    }
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
        let d = Depths { early: 10, full: 100, payout: 100 };
        assert_eq!(depth(100, 108, &d), Depth::Unconfirmed);
        assert_eq!(depth(100, 109, &d), Depth::Early);
        assert_eq!(depth(100, 199, &d), Depth::Full);
        assert_eq!(depth(100, 50, &d), Depth::Unconfirmed);
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
