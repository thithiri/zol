//! A tiny, fully offline Orchard note commitment tree (depth 32).
//!
//! We compute every layer explicitly, so we can produce an authentication path for any
//! leaf. The root is cross-checked against `incrementalmerkletree`'s `Frontier`.

use incrementalmerkletree::{frontier::Frontier, Hashable, Level};
use orchard::{
    note::ExtractedNoteCommitment,
    tree::{MerkleHashOrchard, MerklePath},
    Anchor,
};

pub const DEPTH: u8 = 32;

#[derive(Default, Clone)]
pub struct Tree {
    leaves: Vec<MerkleHashOrchard>,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a note commitment; returns its position.
    pub fn append(&mut self, cmx: &ExtractedNoteCommitment) -> u32 {
        self.leaves.push(MerkleHashOrchard::from_cmx(cmx));
        (self.leaves.len() - 1) as u32
    }

    /// Appends `n` random filler leaves (other people's notes).
    pub fn append_filler(&mut self, n: usize, rng: &mut impl rand::RngCore) {
        for _ in 0..n {
            let mut b = [0u8; 64];
            rng.fill_bytes(&mut b);
            let base = <pasta_curves::pallas::Base as ff::FromUniformBytes<64>>::from_uniform_bytes(&b);
            let cmx = ExtractedNoteCommitment::from_bytes(&ff::PrimeField::to_repr(&base)).unwrap();
            self.append(&cmx);
        }
    }

    fn layers(&self) -> Vec<Vec<MerkleHashOrchard>> {
        let mut layers = vec![self.leaves.clone()];
        for l in 0..DEPTH {
            let cur = layers.last().unwrap();
            let empty = MerkleHashOrchard::empty_root(Level::from(l));
            let next: Vec<_> = cur
                .chunks(2)
                .map(|c| {
                    let r = c.get(1).copied().unwrap_or(empty);
                    MerkleHashOrchard::combine(Level::from(l), &c[0], &r)
                })
                .collect();
            layers.push(next);
        }
        layers
    }

    pub fn root(&self) -> Anchor {
        let layers = self.layers();
        let root = layers[DEPTH as usize]
            .first()
            .copied()
            .unwrap_or_else(|| MerkleHashOrchard::empty_root(Level::from(DEPTH)));
        // Cross-check against incrementalmerkletree's frontier implementation.
        let mut f = Frontier::<MerkleHashOrchard, DEPTH>::empty();
        for leaf in &self.leaves {
            assert!(f.append(*leaf));
        }
        assert_eq!(f.root(), root, "tree root disagrees with incrementalmerkletree::Frontier");
        root.into()
    }

    pub fn path(&self, pos: u32) -> MerklePath {
        let layers = self.layers();
        let mut auth = [MerkleHashOrchard::empty_root(Level::from(0)); DEPTH as usize];
        let mut idx = pos as usize;
        for l in 0..DEPTH as usize {
            let sib = idx ^ 1;
            auth[l] = layers[l]
                .get(sib)
                .copied()
                .unwrap_or_else(|| MerkleHashOrchard::empty_root(Level::from(l as u8)));
            idx >>= 1;
        }
        MerklePath::from_parts(pos, auth)
    }
}
