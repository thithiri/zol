//! Block scanner for vault (and wallet) accounts.
//!
//! - Ingests blocks strictly in order; a block whose `prev_hash` does not match the scanned
//!   tip is rejected with [`Error::Reorg`] and the caller rewinds ([`Scanner::rewind`] or
//!   [`Scanner::sync`], which finds the fork point itself).
//! - Trial-decrypts every Ironwood output with each account's external and internal `ivk`
//!   (any diversifier). Orchard-pool outputs are scanned too when
//!   [`ScannerConfig::scan_orchard_pool`] is set; ZIP 326 says `use_qsk = true` keys SHOULD
//!   NOT scan the Orchard pool, and after NU6.3 nothing new can reach it, so it is off by default.
//! - Tracks the nullifiers of known notes in every Orchard-protocol bundle to detect spends.
//! - Maintains the Ironwood commitment tree (all outputs, ours marked) with one checkpoint per
//!   block, so a note can be spent later (`spendable`) and witnesses survive rewinds.
//! - `snapshot()` / `restore()` serialize the whole state (keys excluded) with bincode.

use std::collections::BTreeMap;

use incrementalmerkletree::frontier::Frontier;
use orchard::{
    keys::{FullViewingKey, PreparedIncomingViewingKey, Scope},
    note::{ExtractedNoteCommitment, Nullifier, RandomSeed, Rho},
    note_encryption::{IronwoodDomain, OrchardDomain},
    tree::{MerkleHashOrchard, MerklePath},
    value::NoteValue,
    Address, Anchor, Note,
};
use serde::{Deserialize, Serialize};
use zcash_note_encryption::try_note_decryption;
use zcash_primitives::transaction::Transaction;

use crate::{
    block::{parse_block, ParsedBlock},
    keys::AppKeys,
    params::ChainParams,
    serde_arr,
    tree::{NoteTree, TreeSnapshot},
    zebra::ZebraClient,
    Error,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Pool {
    Orchard,
    Ironwood,
}

/// Something the scanner watches: an app vault (id = app program ID) or a wallet.
#[derive(Clone, Debug)]
pub struct Account {
    pub id: [u8; 32],
    pub fvk: FullViewingKey,
}

impl From<&AppKeys> for Account {
    fn from(k: &AppKeys) -> Self {
        Self {
            id: k.app_id,
            fvk: k.fvk().clone(),
        }
    }
}

mod scope_serde {
    use orchard::keys::Scope;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(s: &Scope, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_u8(matches!(s, Scope::Internal) as u8)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Scope, D::Error> {
        Ok(if u8::deserialize(d)? == 1 {
            Scope::Internal
        } else {
            Scope::External
        })
    }
}

/// A note received by a watched account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedNote {
    /// The account (app program ID, or wallet id) that received it.
    pub app_id: [u8; 32],
    pub pool: Pool,
    pub cmx: [u8; 32],
    pub nullifier: [u8; 32],
    pub value: u64,
    #[serde(with = "serde_arr")]
    pub memo: [u8; 512],
    #[serde(with = "serde_arr")]
    pub receiver: [u8; 43],
    pub receiver_index: [u8; 11],
    #[serde(with = "scope_serde")]
    pub scope: Scope,
    pub height: u32,
    /// Internal byte order; [`crate::display_hex`] for RPC form.
    pub block_hash: [u8; 32],
    pub txid: [u8; 32],
    pub action_index: u32,
    /// Position in the Ironwood commitment tree (None for Orchard-pool notes, which this
    /// scanner does not witness).
    pub position: Option<u64>,
    rho: [u8; 32],
    rseed: [u8; 32],
}

impl ScannedNote {
    /// Rebuilds the `orchard::Note`.
    pub fn note(&self) -> Note {
        let rho = Rho::from_bytes(&self.rho).unwrap();
        let version = match self.pool {
            Pool::Orchard => orchard::note::NoteVersion::V2,
            Pool::Ironwood => orchard::note::NoteVersion::V3,
        };
        Note::from_parts(
            Address::from_raw_address_bytes(&self.receiver).unwrap(),
            NoteValue::from_raw(self.value),
            rho,
            RandomSeed::from_bytes(self.rseed, &rho).unwrap(),
            version,
        )
        .unwrap()
    }

    /// Decodes a deposit memo in either form (binary `0xFF` or `ZOL1:` text).
    pub fn deposit_memo(&self) -> Result<zol_wire::DepositMemo, zol_wire::MemoError> {
        zol_wire::DepositMemo::decode_any(&self.memo)
    }
}

/// A watched note whose nullifier appeared on chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpentNote {
    pub app_id: [u8; 32],
    pub pool: Pool,
    pub cmx: [u8; 32],
    pub nullifier: [u8; 32],
    pub spending_txid: [u8; 32],
    pub height: u32,
    pub block_hash: [u8; 32],
}

/// What one block changed.
#[derive(Clone, Debug, Default)]
pub struct BlockEvents {
    pub height: u32,
    pub hash: [u8; 32],
    pub received: Vec<ScannedNote>,
    pub spent: Vec<SpentNote>,
    /// `received` and `spent` merged in chain order (transaction order; within a
    /// transaction, spends before outputs; Ironwood bundle before Orchard bundle).
    pub items: Vec<BlockItem>,
}

/// One watched-note event inside a block, in chain order.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum BlockItem {
    Received(ScannedNote),
    Spent(SpentNote),
}

/// One step of [`Scanner::sync`], in the order it happened.
#[derive(Clone, Debug)]
pub enum SyncStep {
    /// Blocks above this height were disconnected.
    Rewind(u32),
    /// A block was connected.
    Block(BlockEvents),
}

/// A note with everything needed to spend it.
#[derive(Clone, Debug)]
pub struct Spendable {
    pub note: Note,
    pub scanned: ScannedNote,
    pub path: MerklePath,
    pub anchor: Anchor,
    pub anchor_height: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ScannerConfig {
    pub params: ChainParams,
    pub scan_orchard_pool: bool,
    /// Blocks kept for rewinds (hashes and tree checkpoints). Zebra keeps 1,000.
    pub max_reorg_depth: u32,
}

impl ScannerConfig {
    pub fn new(params: ChainParams) -> Self {
        Self {
            params,
            scan_orchard_pool: false,
            max_reorg_depth: 1_000,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    pub blocks: u32,
    /// The last rewind of this call, if any.
    pub rewound_to: Option<u32>,
    /// Connected blocks (including any later disconnected within this call; see `steps`).
    pub events: Vec<BlockEvents>,
    /// Rewinds and connected blocks in the order they happened.
    pub steps: Vec<SyncStep>,
}

struct Watched {
    account: Account,
    ivks: [(
        Scope,
        PreparedIncomingViewingKey,
        orchard::keys::IncomingViewingKey,
    ); 2],
}

pub struct Scanner {
    config: ScannerConfig,
    accounts: Vec<Watched>,
    start_height: u32,
    /// Last scanned block (height, hash); before the first block, (start_height - 1, prev hash or zero).
    tip: (u32, [u8; 32]),
    hashes: BTreeMap<u32, [u8; 32]>,
    /// Unspent and spent watched notes, keyed by nullifier.
    notes: BTreeMap<[u8; 32], ScannedNote>,
    spent: BTreeMap<[u8; 32], SpentNote>,
    /// Nullifiers of spent notes whose tree mark was released (spend beyond the reorg window).
    released: std::collections::BTreeSet<[u8; 32]>,
    tree: NoteTree,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    config: ScannerConfig,
    account_ids: Vec<[u8; 32]>,
    start_height: u32,
    tip: (u32, [u8; 32]),
    hashes: Vec<(u32, [u8; 32])>,
    notes: Vec<ScannedNote>,
    spent: Vec<SpentNote>,
    released: Vec<[u8; 32]>,
    tree: TreeSnapshot,
}

const SNAPSHOT_VERSION: u32 = 1;

fn watched(a: Account) -> Watched {
    let ext = a.fvk.to_ivk(Scope::External);
    let int = a.fvk.to_ivk(Scope::Internal);
    Watched {
        ivks: [
            (Scope::External, ext.prepare(), ext),
            (Scope::Internal, int.prepare(), int),
        ],
        account: a,
    }
}

impl Scanner {
    /// A scanner whose first block is `start_height`. `prev_hash` is the hash of block
    /// `start_height - 1` (internal order) if known, so the first block is linked too.
    /// `ironwood_frontier` is the Ironwood tree as of the end of `start_height - 1`
    /// (`None` = empty, correct when NU6.3 activates at or after `start_height`, e.g. regtest).
    pub fn new(
        config: ScannerConfig,
        accounts: Vec<Account>,
        start_height: u32,
        prev_hash: Option<[u8; 32]>,
        ironwood_frontier: Option<Frontier<MerkleHashOrchard, 32>>,
    ) -> Result<Self, Error> {
        let base = start_height
            .checked_sub(1)
            .ok_or(Error::Scan("start_height must be >= 1".into()))?;
        let tree = NoteTree::new(
            config.max_reorg_depth as usize + 10,
            base,
            ironwood_frontier.unwrap_or_else(Frontier::empty),
        )?;
        let mut hashes = BTreeMap::new();
        if let Some(h) = prev_hash {
            hashes.insert(base, h);
        }
        Ok(Self {
            config,
            accounts: accounts.into_iter().map(watched).collect(),
            start_height,
            tip: (base, prev_hash.unwrap_or([0; 32])),
            hashes,
            notes: BTreeMap::new(),
            spent: BTreeMap::new(),
            released: Default::default(),
            tree,
        })
    }

    /// Starts at `start_height` from Zebra's `z_gettreestate` for `start_height - 1`.
    pub async fn from_zebra(
        config: ScannerConfig,
        accounts: Vec<Account>,
        client: &ZebraClient,
        start_height: u32,
    ) -> Result<Self, Error> {
        let base = start_height
            .checked_sub(1)
            .ok_or(Error::Scan("start_height must be >= 1".into()))?;
        let prev = crate::from_display_hex(&client.get_block_hash(base).await?)?;
        let frontier = if base == 0 {
            None
        } else {
            let ts = client.z_get_treestate(&base.to_string()).await?;
            match ts
                .pointer("/ironwood/commitments/finalState")
                .and_then(|v| v.as_str())
            {
                None => None,
                Some(hexs) => Some(parse_legacy_tree(
                    &hex::decode(hexs).map_err(|e| Error::Parse(e.to_string()))?,
                )?),
            }
        };
        Self::new(config, accounts, start_height, Some(prev), frontier)
    }

    /// Adds an account. It is only scanned from the next block on.
    pub fn add_account(&mut self, a: Account) {
        if !self.accounts.iter().any(|w| w.account.id == a.id) {
            self.accounts.push(watched(a));
        }
    }

    pub fn config(&self) -> &ScannerConfig {
        &self.config
    }

    /// Last scanned block (height, hash).
    pub fn tip(&self) -> (u32, [u8; 32]) {
        self.tip
    }

    pub fn next_height(&self) -> u32 {
        self.tip.0 + 1
    }

    /// Every received note, spent or not.
    pub fn all_notes(&self) -> impl Iterator<Item = &ScannedNote> {
        self.notes.values()
    }

    /// Unspent notes of an account.
    pub fn unspent(&self, app_id: &[u8; 32]) -> impl Iterator<Item = &ScannedNote> {
        let id = *app_id;
        self.notes
            .values()
            .filter(move |n| n.app_id == id && !self.spent.contains_key(&n.nullifier))
    }

    pub fn spent_notes(&self) -> impl Iterator<Item = &SpentNote> {
        self.spent.values()
    }

    pub fn note_by_cmx(&self, cmx: &[u8; 32]) -> Option<&ScannedNote> {
        self.notes.values().find(|n| &n.cmx == cmx)
    }

    pub fn spend_of(&self, cmx: &[u8; 32]) -> Option<&SpentNote> {
        self.spent.values().find(|s| &s.cmx == cmx)
    }

    /// Ironwood anchor (tree root) at the end of block `height`, if still held.
    pub fn anchor_at(&self, height: u32) -> Result<Option<Anchor>, Error> {
        self.tree.root_at(height)
    }

    /// Note, witness and anchor to spend the Ironwood note `cmx`, anchored at
    /// `anchor_height` (default: the tip).
    pub fn spendable(
        &self,
        cmx: &[u8; 32],
        anchor_height: Option<u32>,
    ) -> Result<Spendable, Error> {
        let n = self
            .note_by_cmx(cmx)
            .ok_or(Error::Scan("unknown note".into()))?;
        if self.spent.contains_key(&n.nullifier) {
            return Err(Error::Scan("note already spent".into()));
        }
        let pos = n
            .position
            .ok_or(Error::Scan("not an Ironwood note (no witness)".into()))?;
        let h = anchor_height.unwrap_or(self.tip.0);
        if h < n.height || h > self.tip.0 {
            return Err(Error::Scan(
                "anchor height outside [note height, tip]".into(),
            ));
        }
        let path = self
            .tree
            .witness(pos, h)?
            .ok_or(Error::Scan("no witness at that height".into()))?;
        let anchor = self
            .tree
            .root_at(h)?
            .ok_or(Error::Scan("no anchor at that height".into()))?;
        let note = n.note();
        if path.root(ExtractedNoteCommitment::from(note.commitment())) != anchor {
            return Err(Error::Scan("witness does not root to anchor".into()));
        }
        Ok(Spendable {
            note,
            scanned: n.clone(),
            path,
            anchor,
            anchor_height: h,
        })
    }

    /// Parses and ingests a raw block (must be `next_height()`).
    pub fn ingest_block(&mut self, raw: &[u8]) -> Result<BlockEvents, Error> {
        let b = parse_block(raw, &self.config.params)?;
        self.ingest_parsed(&b)
    }

    pub fn ingest_parsed(&mut self, b: &ParsedBlock) -> Result<BlockEvents, Error> {
        if b.height != self.next_height() {
            return Err(Error::Scan(format!(
                "expected block {}, got {}",
                self.next_height(),
                b.height
            )));
        }
        let linked = match self.hashes.get(&self.tip.0) {
            Some(h) => *h == b.prev_hash,
            None => true, // unknown predecessor (no prev_hash given at start)
        };
        if !linked {
            return Err(Error::Reorg { height: b.height });
        }
        let mut ev = BlockEvents {
            height: b.height,
            hash: b.hash,
            ..Default::default()
        };
        let mut leaves: Vec<(MerkleHashOrchard, bool)> = vec![];
        let mut ours_at_leaf: Vec<(usize, ScannedNote)> = vec![];
        for tx in &b.txs {
            self.scan_tx(tx, b, &mut ev, &mut leaves, &mut ours_at_leaf)?;
        }
        let positions = self.tree.append_block(b.height, &leaves)?;
        for (leaf_idx, n) in ours_at_leaf {
            for it in ev.items.iter_mut() {
                if let BlockItem::Received(x) = it {
                    if x.pool == Pool::Ironwood
                        && x.cmx == n.cmx
                        && x.action_index == n.action_index
                        && x.txid == n.txid
                    {
                        x.position = Some(positions[leaf_idx]);
                    }
                }
            }
        }
        for it in &ev.items {
            if let BlockItem::Received(n) = it {
                ev.received.push(n.clone());
                self.notes.insert(n.nullifier, n.clone());
            }
        }
        self.tip = (b.height, b.hash);
        self.hashes.insert(b.height, b.hash);
        let keep_from = b.height.saturating_sub(self.config.max_reorg_depth);
        self.hashes = self.hashes.split_off(&keep_from);
        // Release tree marks of notes spent beyond the reorg window (their witnesses are no
        // longer needed), so the tree can prune.
        let done: Vec<_> = self
            .spent
            .values()
            .filter(|x| x.height < keep_from && !self.released.contains(&x.nullifier))
            .map(|x| x.nullifier)
            .collect();
        for nf in done {
            if let Some(pos) = self.notes.get(&nf).and_then(|n| n.position) {
                self.tree.unmark(pos, b.height)?;
            }
            self.released.insert(nf);
        }
        Ok(ev)
    }

    fn scan_tx(
        &mut self,
        tx: &Transaction,
        b: &ParsedBlock,
        ev: &mut BlockEvents,
        leaves: &mut Vec<(MerkleHashOrchard, bool)>,
        ours_at_leaf: &mut Vec<(usize, ScannedNote)>,
    ) -> Result<(), Error> {
        let txid: [u8; 32] = *tx.txid().as_ref();
        for (pool, bundle) in [
            (Pool::Ironwood, tx.ironwood_bundle()),
            (Pool::Orchard, tx.orchard_bundle()),
        ] {
            let Some(bundle) = bundle else { continue };
            // Spends first (a note cannot be created and spent in one block, but be safe).
            for a in bundle.actions() {
                let nf = a.nullifier().to_bytes();
                if let Some(n) = self.notes.get(&nf) {
                    if let std::collections::btree_map::Entry::Vacant(e) = self.spent.entry(nf) {
                        let s = SpentNote {
                            app_id: n.app_id,
                            pool,
                            cmx: n.cmx,
                            nullifier: nf,
                            spending_txid: txid,
                            height: b.height,
                            block_hash: b.hash,
                        };
                        ev.spent.push(s.clone());
                        ev.items.push(BlockItem::Spent(s.clone()));
                        e.insert(s);
                    }
                }
            }
            for (i, a) in bundle.actions().iter().enumerate() {
                let cmx = a.cmx().to_bytes();
                let found = if pool == Pool::Ironwood || self.config.scan_orchard_pool {
                    self.try_decrypt(pool, a)
                } else {
                    None
                };
                let found = found.map(|(acct, scope, idx, note, memo)| {
                    let fvk = &self.accounts[acct].account.fvk;
                    ScannedNote {
                        app_id: self.accounts[acct].account.id,
                        pool,
                        cmx,
                        nullifier: note.nullifier(fvk).to_bytes(),
                        value: note.value().inner(),
                        memo,
                        receiver: note.recipient().to_raw_address_bytes(),
                        receiver_index: idx,
                        scope,
                        height: b.height,
                        block_hash: b.hash,
                        txid,
                        action_index: i as u32,
                        position: None,
                        rho: note.rho().to_bytes(),
                        rseed: *note.rseed().as_bytes(),
                    }
                });
                if pool == Pool::Ironwood {
                    let leaf = MerkleHashOrchard::from_cmx(
                        &ExtractedNoteCommitment::from_bytes(&cmx).unwrap(),
                    );
                    leaves.push((leaf, found.is_some()));
                    if let Some(n) = &found {
                        ours_at_leaf.push((leaves.len() - 1, n.clone()));
                    }
                }
                if let Some(n) = found {
                    ev.items.push(BlockItem::Received(n));
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn try_decrypt<T>(
        &self,
        pool: Pool,
        a: &orchard::Action<T>,
    ) -> Option<(usize, Scope, [u8; 11], Note, [u8; 512])> {
        for (ai, w) in self.accounts.iter().enumerate() {
            for (scope, pivk, ivk) in &w.ivks {
                let r = match pool {
                    Pool::Ironwood => try_note_decryption(&IronwoodDomain::for_action(a), pivk, a),
                    Pool::Orchard => try_note_decryption(&OrchardDomain::for_action(a), pivk, a),
                };
                if let Some((note, addr, memo)) = r {
                    let idx = ivk
                        .diversifier_index(&addr)
                        .map(|d| *d.as_bytes())
                        .unwrap_or([0; 11]);
                    return Some((ai, *scope, idx, note, memo));
                }
            }
        }
        None
    }

    /// Drops every block above `to_height`: notes received there are forgotten, spends
    /// recorded there are undone, the commitment tree is truncated to its checkpoint. Notes
    /// at or below `to_height` keep their witnesses.
    pub fn rewind(&mut self, to_height: u32) -> Result<(), Error> {
        if to_height >= self.tip.0 {
            return Ok(());
        }
        if to_height + 1 < self.start_height {
            return Err(Error::Scan("cannot rewind below the scan start".into()));
        }
        if !self.tree.rewind(to_height)? {
            return Err(Error::Scan(format!(
                "rewind to {to_height} is deeper than the retained window (oldest checkpoint {:?}): global halt",
                self.tree.oldest_checkpoint()
            )));
        }
        self.notes.retain(|_, n| n.height <= to_height);
        self.spent.retain(|_, s| s.height <= to_height);
        self.hashes = self
            .hashes
            .split_off(&0)
            .into_iter()
            .filter(|(h, _)| *h <= to_height)
            .collect();
        let hash = self.hashes.get(&to_height).copied().unwrap_or([0; 32]);
        self.tip = (to_height, hash);
        Ok(())
    }

    /// Scans from `next_height()` to Zebra's tip (or `until`), detecting and handling
    /// reorgs by walking back to the last block whose hash Zebra still agrees with.
    pub async fn sync(
        &mut self,
        client: &ZebraClient,
        until: Option<u32>,
    ) -> Result<SyncReport, Error> {
        let mut rep = SyncReport::default();
        // Check our tip is still on the node's chain; walk back if not.
        loop {
            let (h, hash) = self.tip;
            if h < self.start_height || !self.hashes.contains_key(&h) {
                break;
            }
            let theirs = crate::from_display_hex(&client.get_block_hash(h).await?)?;
            if theirs == hash {
                break;
            }
            let fork = self.find_fork(client).await?;
            self.rewind(fork)?;
            rep.rewound_to = Some(fork);
            rep.steps.push(SyncStep::Rewind(fork));
        }
        let target = match until {
            Some(u) => u,
            None => client.get_block_count().await?,
        };
        while self.next_height() <= target {
            let raw = client.get_block_raw_by_height(self.next_height()).await?;
            match self.ingest_block(&raw) {
                Ok(ev) => {
                    rep.blocks += 1;
                    rep.events.push(ev.clone());
                    rep.steps.push(SyncStep::Block(ev));
                }
                Err(Error::Reorg { .. }) => {
                    let fork = self.find_fork(client).await?;
                    self.rewind(fork)?;
                    rep.rewound_to = Some(fork);
                    rep.steps.push(SyncStep::Rewind(fork));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(rep)
    }

    async fn find_fork(&self, client: &ZebraClient) -> Result<u32, Error> {
        for (h, ours) in self.hashes.iter().rev() {
            let theirs = crate::from_display_hex(&client.get_block_hash(*h).await?)?;
            if theirs == *ours {
                return Ok(*h);
            }
        }
        Err(Error::Scan(
            "no common block within the retained window: global halt".into(),
        ))
    }

    /// Serializes the scanner state (not the keys).
    pub fn snapshot(&self) -> Result<Vec<u8>, Error> {
        let s = Snapshot {
            version: SNAPSHOT_VERSION,
            config: self.config,
            account_ids: self.accounts.iter().map(|w| w.account.id).collect(),
            start_height: self.start_height,
            tip: self.tip,
            hashes: self.hashes.iter().map(|(h, x)| (*h, *x)).collect(),
            notes: self.notes.values().cloned().collect(),
            spent: self.spent.values().cloned().collect(),
            released: self.released.iter().copied().collect(),
            tree: self.tree.snapshot()?,
        };
        bincode::serialize(&s).map_err(|e| Error::Scan(e.to_string()))
    }

    /// The account ids recorded in a snapshot (to rebuild the matching `Account`s).
    pub fn snapshot_account_ids(bytes: &[u8]) -> Result<Vec<[u8; 32]>, Error> {
        let s: Snapshot = bincode::deserialize(bytes).map_err(|e| Error::Scan(e.to_string()))?;
        Ok(s.account_ids)
    }

    /// Restores a snapshot. `accounts` must contain exactly the snapshot's account ids.
    pub fn restore(bytes: &[u8], accounts: Vec<Account>) -> Result<Self, Error> {
        let s: Snapshot = bincode::deserialize(bytes).map_err(|e| Error::Scan(e.to_string()))?;
        if s.version != SNAPSHOT_VERSION {
            return Err(Error::Scan(format!("snapshot version {}", s.version)));
        }
        let mut ids: Vec<_> = accounts.iter().map(|a| a.id).collect();
        let mut want = s.account_ids.clone();
        ids.sort();
        want.sort();
        if ids != want {
            return Err(Error::Scan("accounts do not match the snapshot".into()));
        }
        // Keep the snapshot's account order.
        let mut by_id: BTreeMap<_, _> = accounts.into_iter().map(|a| (a.id, a)).collect();
        let accounts = s
            .account_ids
            .iter()
            .map(|id| watched(by_id.remove(id).unwrap()))
            .collect();
        Ok(Self {
            config: s.config,
            accounts,
            start_height: s.start_height,
            tip: s.tip,
            hashes: s.hashes.into_iter().collect(),
            notes: s.notes.into_iter().map(|n| (n.nullifier, n)).collect(),
            spent: s.spent.into_iter().map(|x| (x.nullifier, x)).collect(),
            released: s.released.into_iter().collect(),
            tree: NoteTree::restore(&s.tree)?,
        })
    }
}

/// Parses a `z_gettreestate` `finalState` (legacy `CommitmentTree` encoding) into a frontier.
pub fn parse_legacy_tree(bytes: &[u8]) -> Result<Frontier<MerkleHashOrchard, 32>, Error> {
    let t = zcash_primitives::merkle_tree::read_commitment_tree::<MerkleHashOrchard, _, 32>(bytes)
        .map_err(|e| Error::Parse(format!("treestate: {e}")))?;
    Ok(t.to_frontier())
}

#[allow(dead_code)]
fn _nf(n: &Nullifier) -> [u8; 32] {
    n.to_bytes()
}

#[cfg(test)]
mod send_tests {
    use super::*;
    fn assert_send<T: Send>(_: &T) {}
    #[allow(dead_code)]
    fn sync_future_is_send(s: &mut Scanner, c: &ZebraClient) {
        assert_send(&s.sync(c, None));
    }
    #[test]
    fn scanner_is_send_sync() {
        fn ss<T: Send + Sync>() {}
        ss::<Scanner>();
        ss::<ZebraClient>();
        ss::<crate::AppKeys>();
    }
}
