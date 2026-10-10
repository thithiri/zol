//! The Ironwood note commitment tree as a checkpointed `shardtree`, one checkpoint per
//! scanned block (id = height), with marked leaves for the notes we may spend. Rewinding to
//! a checkpoint discards everything after it while keeping earlier marks, so witnesses of
//! notes below the rewind height survive a reorg. Serializable for persistence.

use incrementalmerkletree::{frontier::Frontier, Address, Level, Marking, Position, Retention};
use orchard::tree::{MerkleHashOrchard, MerklePath};
use serde::{Deserialize, Serialize};
use shardtree::{
    store::{memory::MemoryShardStore, Checkpoint, ShardStore, TreeState},
    LocatedPrunableTree, Node, PrunableTree, RetentionFlags, ShardTree,
};

use crate::Error;

type H = MerkleHashOrchard;
type Store = MemoryShardStore<H, u32>;
const DEPTH: u8 = 32;
const SHARD_HEIGHT: u8 = 16;

pub(crate) struct NoteTree {
    inner: ShardTree<Store, DEPTH, SHARD_HEIGHT>,
    max_checkpoints: usize,
}

fn terr(e: impl core::fmt::Debug) -> Error {
    Error::Tree(format!("{e:?}"))
}

impl NoteTree {
    /// An empty tree with a checkpoint at `height` (the scan's starting point), or a tree
    /// initialised from a frontier (e.g. `z_gettreestate` at the birthday).
    pub fn new(
        max_checkpoints: usize,
        height: u32,
        frontier: Frontier<H, DEPTH>,
    ) -> Result<Self, Error> {
        let mut inner = ShardTree::new(Store::empty(), max_checkpoints);
        inner
            .insert_frontier(
                frontier,
                Retention::Checkpoint {
                    id: height,
                    marking: Marking::None,
                },
            )
            .map_err(terr)?;
        Ok(Self {
            inner,
            max_checkpoints,
        })
    }

    /// Appends one block's leaves (`true` = ours, keep a witness) and checkpoints the block.
    /// Returns the position of every leaf.
    pub fn append_block(&mut self, height: u32, leaves: &[(H, bool)]) -> Result<Vec<u64>, Error> {
        let mut out = Vec::with_capacity(leaves.len());
        for (h, marked) in leaves {
            let r = if *marked {
                Retention::Marked
            } else {
                Retention::Ephemeral
            };
            self.inner.append(*h, r).map_err(terr)?;
            out.push(u64::from(
                self.inner
                    .max_leaf_position(None)
                    .map_err(terr)?
                    .expect("just appended"),
            ));
        }
        if !self.inner.checkpoint(height).map_err(terr)? {
            return Err(Error::Tree(format!(
                "checkpoint {height} not added (not increasing?)"
            )));
        }
        Ok(out)
    }

    /// Discards everything after the checkpoint for `height`. `false` if that checkpoint is
    /// not (or no longer) held: the reorg is deeper than the retained window.
    pub fn rewind(&mut self, height: u32) -> Result<bool, Error> {
        self.inner.truncate_to_checkpoint(&height).map_err(terr)
    }

    pub fn root_at(&self, height: u32) -> Result<Option<orchard::Anchor>, Error> {
        Ok(self
            .inner
            .root_at_checkpoint_id(&height)
            .map_err(terr)?
            .map(orchard::Anchor::from))
    }

    pub fn witness(&self, position: u64, height: u32) -> Result<Option<MerklePath>, Error> {
        Ok(self
            .inner
            .witness_at_checkpoint_id(Position::from(position), &height)
            .map_err(terr)?
            .map(MerklePath::from))
    }

    pub fn unmark(&mut self, position: u64, as_of: u32) -> Result<(), Error> {
        self.inner
            .remove_mark(Position::from(position), Some(&as_of))
            .map_err(terr)?;
        Ok(())
    }

    pub fn oldest_checkpoint(&self) -> Option<u32> {
        self.inner.store().min_checkpoint_id().ok().flatten()
    }

    pub fn snapshot(&self) -> Result<TreeSnapshot, Error> {
        let st = self.inner.store();
        let mut shards = vec![];
        for addr in st.get_shard_roots().map_err(terr)? {
            if let Some(s) = st.get_shard(addr).map_err(terr)? {
                shards.push((
                    u8::from(addr.level()),
                    addr.index(),
                    SNode::from_tree(s.root()),
                ));
            }
        }
        let cap = SNode::from_tree(&st.get_cap().map_err(terr)?);
        let mut checkpoints = vec![];
        st.for_each_checkpoint(usize::MAX, |id, cp| {
            checkpoints.push(SCheckpoint {
                id: *id,
                position: match cp.tree_state() {
                    TreeState::Empty => None,
                    TreeState::AtPosition(p) => Some(u64::from(p)),
                },
                marks_removed: cp.marks_removed().iter().map(|p| u64::from(*p)).collect(),
            });
            Ok(())
        })
        .map_err(terr)?;
        Ok(TreeSnapshot {
            max_checkpoints: self.max_checkpoints as u64,
            shards,
            cap,
            checkpoints,
        })
    }

    pub fn restore(s: &TreeSnapshot) -> Result<Self, Error> {
        let mut st = Store::empty();
        for (level, index, node) in &s.shards {
            let addr = Address::from_parts(Level::from(*level), *index);
            let t = LocatedPrunableTree::from_parts(addr, node.to_tree()?).map_err(terr)?;
            st.put_shard(t).map_err(terr)?;
        }
        st.put_cap(s.cap.to_tree()?).map_err(terr)?;
        for c in &s.checkpoints {
            let ts = match c.position {
                None => TreeState::Empty,
                Some(p) => TreeState::AtPosition(Position::from(p)),
            };
            let cp = Checkpoint::from_parts(
                ts,
                c.marks_removed.iter().map(|p| Position::from(*p)).collect(),
            );
            st.add_checkpoint(c.id, cp).map_err(terr)?;
        }
        Ok(Self {
            inner: ShardTree::new(st, s.max_checkpoints as usize),
            max_checkpoints: s.max_checkpoints as usize,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeSnapshot {
    max_checkpoints: u64,
    shards: Vec<(u8, u64, SNode)>,
    cap: SNode,
    checkpoints: Vec<SCheckpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SCheckpoint {
    id: u32,
    position: Option<u64>,
    marks_removed: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum SNode {
    Nil,
    Leaf {
        h: [u8; 32],
        flags: u8,
    },
    Parent {
        ann: Option<[u8; 32]>,
        l: Box<SNode>,
        r: Box<SNode>,
    },
}

impl SNode {
    fn from_tree(t: &PrunableTree<H>) -> Self {
        match &**t {
            Node::Nil => SNode::Nil,
            Node::Leaf { value: (h, f) } => SNode::Leaf {
                h: h.to_bytes(),
                flags: f.bits(),
            },
            Node::Parent { ann, left, right } => SNode::Parent {
                ann: ann.as_ref().map(|a| a.to_bytes()),
                l: Box::new(SNode::from_tree(left)),
                r: Box::new(SNode::from_tree(right)),
            },
        }
    }

    fn to_tree(&self) -> Result<PrunableTree<H>, Error> {
        let h = |b: &[u8; 32]| {
            Option::<H>::from(H::from_bytes(b)).ok_or_else(|| Error::Tree("bad node hash".into()))
        };
        Ok(match self {
            SNode::Nil => PrunableTree::empty(),
            SNode::Leaf { h: b, flags } => PrunableTree::leaf((
                h(b)?,
                RetentionFlags::from_bits(*flags).ok_or_else(|| Error::Tree("bad flags".into()))?,
            )),
            SNode::Parent { ann, l, r } => PrunableTree::parent(
                ann.as_ref().map(h).transpose()?.map(std::sync::Arc::new),
                l.to_tree()?,
                r.to_tree()?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use incrementalmerkletree::Hashable;

    fn leaf(i: u64) -> H {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&i.to_le_bytes());
        Option::from(H::from_bytes(&b)).unwrap()
    }

    #[test]
    fn rewind_keeps_old_witness_and_snapshot_roundtrips() {
        let mut t = NoteTree::new(100, 1, Frontier::empty()).unwrap();
        // Block 2: 3 leaves, ours is #1.
        let pos = t
            .append_block(2, &[(leaf(10), false), (leaf(11), true), (leaf(12), false)])
            .unwrap();
        assert_eq!(pos, vec![0, 1, 2]);
        // Blocks 3..=5.
        for h in 3..=5u32 {
            t.append_block(
                h,
                &[
                    (leaf(h as u64 * 100), false),
                    (leaf(h as u64 * 100 + 1), h == 4),
                ],
            )
            .unwrap();
        }
        let w5 = t.witness(1, 5).unwrap().unwrap();
        assert_eq!(
            w5.root(
                orchard::note::ExtractedNoteCommitment::from_bytes(&leaf(11).to_bytes()).unwrap()
            ),
            t.root_at(5).unwrap().unwrap()
        );
        let root3 = t.root_at(3).unwrap().unwrap();

        // Snapshot / restore.
        let snap = t.snapshot().unwrap();
        let bytes = bincode::serialize(&snap).unwrap();
        let mut t2 = NoteTree::restore(&bincode::deserialize(&bytes).unwrap()).unwrap();
        assert_eq!(t2.snapshot().unwrap(), snap);
        assert_eq!(t2.root_at(5).unwrap(), t.root_at(5).unwrap());

        // Rewind to 3: the witness for leaf #1 still works, at the old root.
        assert!(t2.rewind(3).unwrap());
        assert_eq!(t2.root_at(5).unwrap(), None);
        let w3 = t2.witness(1, 3).unwrap().unwrap();
        assert_eq!(
            w3.root(
                orchard::note::ExtractedNoteCommitment::from_bytes(&leaf(11).to_bytes()).unwrap()
            ),
            root3
        );
        // Re-append a different block 4; positions continue from the rewind point.
        let p = t2.append_block(4, &[(leaf(999), false)]).unwrap();
        assert_eq!(p, vec![5]);
        assert!(t2.witness(1, 4).unwrap().is_some());
        // Empty block still checkpoints.
        t2.append_block(5, &[]).unwrap();
        assert!(t2.root_at(5).unwrap().is_some());
        // Rewinding below the window fails.
        assert!(!t2.rewind(0).unwrap());
        let _ = H::empty_leaf();
    }
}
