//! Creating "received" notes offline: we build an (unproven) Orchard/Ironwood bundle that
//! pays the vault address, exactly as a depositor's wallet would, and trial-decrypt it
//! with the vault's incoming viewing key. No proof is needed to obtain the note.

use orchard::{
    builder::{Builder, BundleType, UnauthorizedBundle},
    keys::{IncomingViewingKey, Scope},
    note_encryption::{IronwoodDomain, OrchardDomain},
    value::NoteValue,
    Address, Anchor, Note,
};
use zcash_note_encryption::try_note_decryption;

use crate::pool::Pool;

/// An unproven bundle carrying one output to `recipient` (a "deposit").
pub struct Deposit {
    pub pool: Pool,
    pub bundle: UnauthorizedBundle<i64>,
    pub action_index: usize,
}

pub fn make_deposit(pool: Pool, recipient: Address, value: u64, memo: [u8; 512]) -> Deposit {
    let bv = pool.bundle_version();
    let mut b = Builder::new(BundleType::DEFAULT, bv, bv.default_flags(), Anchor::empty_tree())
        .expect("builder");
    b.add_output(None, recipient, NoteValue::from_raw(value), memo)
        .expect("add_output");
    let (bundle, meta) = b
        .build::<i64>(&mut rand::rngs::OsRng)
        .expect("build")
        .expect("non-empty");
    Deposit {
        pool,
        bundle,
        action_index: meta.output_action_index(0).unwrap(),
    }
}

impl Deposit {
    /// Trial-decrypts the deposit's output with `ivk`.
    pub fn try_decrypt(&self, ivk: &IncomingViewingKey) -> Option<(Note, Address, [u8; 512])> {
        let action = &self.bundle.actions()[self.action_index];
        match self.pool {
            Pool::OrchardNu62 => {
                try_note_decryption(&OrchardDomain::for_action(action), &ivk.prepare(), action)
            }
            Pool::IronwoodNu63 => {
                try_note_decryption(&IronwoodDomain::for_action(action), &ivk.prepare(), action)
            }
        }
    }
}

/// Convenience: deposit `value` to `fvk`'s external address 0 and return the decrypted note.
pub fn receive(pool: Pool, fvk: &orchard::keys::FullViewingKey, value: u64, memo: [u8; 512]) -> Note {
    let addr = fvk.address_at(0u32, Scope::External);
    let d = make_deposit(pool, addr, value, memo);
    let (note, to, _) = d
        .try_decrypt(&fvk.to_ivk(Scope::External))
        .expect("vault ivk decrypts its own deposit");
    assert_eq!(to, addr);
    note
}
