#![allow(dead_code)]
//! Shared fixtures for the spike tests.

use orchard::{
    keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey},
    Address, Note,
};
use rand::RngCore;
use zol_zcash_spikes::{
    notes,
    payout::{self, Built, Output, PayoutRequest},
    pool::Pool,
    tree::Tree,
};

pub fn random_sk() -> SpendingKey {
    loop {
        let mut b = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut b);
        if let Some(sk) = Option::from(SpendingKey::from_bytes(b)) {
            return sk;
        }
    }
}

pub fn external_address() -> Address {
    FullViewingKey::from(&random_sk()).address_at(0u32, Scope::External)
}

/// A 512-byte memo starting with 0xFF (arbitrary data) and a tag.
pub fn memo(tag: &[u8]) -> [u8; 512] {
    let mut m = [0u8; 512];
    m[0] = 0xFF;
    m[1..1 + tag.len()].copy_from_slice(tag);
    m
}

pub struct Fixture {
    pub pool: Pool,
    pub vault_fvk: FullViewingKey,
    pub head: Note,
    pub tree: Tree,
    pub head_pos: u32,
    pub payout_fvk: FullViewingKey,
    pub payout_to: Address,
    pub payout_value: u64,
    pub head_memo: [u8; 512],
    pub payout_memo: [u8; 512],
    pub built: Built,
}

pub const HEAD_VALUE: u64 = 1_000_000;
pub const PAYOUT_VALUE: u64 = 600_000;

/// Receive one head note into `vault_fvk`, put it in a tree with filler leaves, and
/// build the one-input payout PCZT (Creator stage).
pub fn fixture(pool: Pool, vault_fvk: FullViewingKey) -> Fixture {
    fixture_with_padding(pool, vault_fvk, vec![])
}

pub fn fixture_with_padding(pool: Pool, vault_fvk: FullViewingKey, padding: Vec<Output>) -> Fixture {
    let head = notes::receive(pool, &vault_fvk, HEAD_VALUE, memo(b"ZOLHEAD genesis"));
    let mut tree = Tree::new();
    tree.append_filler(5, &mut rand::rngs::OsRng);
    let head_pos = tree.append(&head.commitment().into());
    tree.append_filler(3, &mut rand::rngs::OsRng);
    let anchor = tree.root();
    let path = tree.path(head_pos);
    assert_eq!(path.root(head.commitment().into()), anchor, "path roots to anchor");

    let payout_fvk = FullViewingKey::from(&random_sk());
    let payout_to = payout_fvk.address_at(0u32, Scope::External);
    let head_memo = memo(b"ZOLHEAD position=1 last_paid_seq=1");
    let payout_memo = memo(b"ZOLPAY seq=1");
    let req = PayoutRequest {
        pool,
        vault_fvk: vault_fvk.clone(),
        spends: vec![(head, path)],
        anchor,
        payouts: vec![Output {
            to: payout_to,
            value: PAYOUT_VALUE,
            ovk: Some(vault_fvk.to_ovk(Scope::External)),
            memo: payout_memo,
        }],
        head_to: vault_fvk.address_at(0u32, Scope::Internal),
        head_ovk: Some(vault_fvk.to_ovk(Scope::Internal)),
        head_memo,
        padding,
    };
    let built = payout::build(&req);
    Fixture {
        pool,
        vault_fvk,
        head,
        tree,
        head_pos,
        payout_fvk,
        payout_to,
        payout_value: PAYOUT_VALUE,
        head_memo,
        payout_memo,
        built,
    }
}

pub fn single_key_vault() -> (SpendAuthorizingKey, FullViewingKey) {
    let sk = random_sk();
    (SpendAuthorizingKey::from(&sk), FullViewingKey::from(&sk))
}

pub fn hex32(b: &[u8]) -> String {
    hex::encode(b)
}
