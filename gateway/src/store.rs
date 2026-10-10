//! Durable per-operator state (SQLite).
//!
//! Nothing here prevents paying twice; that is Zcash's job (the vault head). The
//! store holds what this operator's own scan saw, the decisions its attestor key
//! has signed, deposit intents registered by app pages, and the halt flag.

use rusqlite::{params, Connection, OptionalExtension};

use crate::chain::{AppId, ScannedNote, Scope};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("conflicting decision already recorded for this deposit")]
    Conflict,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// What a note is, from its memo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Deposit,
    Head,
    TopUp,
}

impl NoteKind {
    fn as_str(self) -> &'static str {
        match self {
            NoteKind::Deposit => "deposit",
            NoteKind::Head => "head",
            NoteKind::TopUp => "topup",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "head" => NoteKind::Head,
            "topup" => NoteKind::TopUp,
            _ => NoteKind::Deposit,
        }
    }
}

/// Gateway-side state of a deposit note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteState {
    /// Seen, no decision yet.
    Seen,
    /// No usable memo or refund address; waits for the sender to prove the payment.
    Held,
    /// A credit attestation was signed for it.
    Credited,
    /// A void attestation was signed for it.
    Voided,
}

impl NoteState {
    fn as_str(self) -> &'static str {
        match self {
            NoteState::Seen => "seen",
            NoteState::Held => "held",
            NoteState::Credited => "credited",
            NoteState::Voided => "voided",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "held" => NoteState::Held,
            "credited" => NoteState::Credited,
            "voided" => NoteState::Voided,
            _ => NoteState::Seen,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NoteRecord {
    pub note: ScannedNote,
    pub kind: NoteKind,
    pub deposit_id: [u8; 32],
    pub state: NoteState,
    /// The memo the decision was made from (chain memo or registered intent).
    pub decided_memo: Option<Vec<u8>>,
    /// Digest signed for this note (credit or void).
    pub digest: Option<[u8; 32]>,
    /// Confirmations at the time of the credit decision.
    pub credited_depth: Option<u32>,
    /// The note's block was reorged away after a decision was signed. The record is
    /// kept so reconciliation still matches what was attested.
    pub rolled_back: bool,
    pub spent: Option<([u8; 32], u32)>,
}

pub struct Store {
    db: Connection,
}

fn b32(v: Vec<u8>) -> [u8; 32] {
    v.try_into().unwrap_or([0; 32])
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let db = Connection::open(path)?;
        Self::init(db)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self> {
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS notes (
                app BLOB NOT NULL, cmx BLOB NOT NULL, deposit_id BLOB NOT NULL, nullifier BLOB NOT NULL,
                value INTEGER NOT NULL, memo BLOB NOT NULL, receiver BLOB NOT NULL, receiver_index TEXT NOT NULL,
                scope TEXT NOT NULL, height INTEGER NOT NULL, block_hash BLOB NOT NULL, txid BLOB NOT NULL,
                kind TEXT NOT NULL, state TEXT NOT NULL, decided_memo BLOB, digest BLOB, credited_depth INTEGER,
                rolled_back INTEGER NOT NULL DEFAULT 0, spent_txid BLOB, spent_height INTEGER,
                PRIMARY KEY (app, cmx));
             CREATE INDEX IF NOT EXISTS notes_deposit ON notes(deposit_id);
             CREATE INDEX IF NOT EXISTS notes_nf ON notes(nullifier);
             CREATE TABLE IF NOT EXISTS decisions (
                key BLOB PRIMARY KEY, kind TEXT NOT NULL, digest BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS intents (
                app BLOB NOT NULL, receiver_index TEXT NOT NULL, session_key BLOB NOT NULL, memo BLOB NOT NULL,
                PRIMARY KEY (app, receiver_index));
             CREATE TABLE IF NOT EXISTS blocks (height INTEGER PRIMARY KEY, hash BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v BLOB NOT NULL);",
        )?;
        Ok(Self { db })
    }

    // ------------------------------------------------------------ notes

    /// Records a newly scanned note. A note already known (same app and cmx) is
    /// left as is, except that a rolled-back one that reappears is restored.
    pub fn insert_note(&self, n: &ScannedNote, kind: NoteKind, deposit_id: &[u8; 32]) -> Result<()> {
        self.db.execute(
            "INSERT INTO notes (app,cmx,deposit_id,nullifier,value,memo,receiver,receiver_index,scope,height,block_hash,txid,kind,state)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'seen')
             ON CONFLICT(app,cmx) DO UPDATE SET rolled_back=0, height=excluded.height, block_hash=excluded.block_hash, txid=excluded.txid",
            params![
                &n.app[..], &n.cmx[..], &deposit_id[..], &n.nullifier[..], n.value as i64, &n.memo, &n.receiver,
                n.receiver_index.to_string(), match n.scope { Scope::External => "external", Scope::Internal => "internal" },
                n.height, &n.block_hash[..], &n.txid[..], kind.as_str()
            ],
        )?;
        Ok(())
    }

    fn row(r: &rusqlite::Row) -> rusqlite::Result<NoteRecord> {
        let spent_txid: Option<Vec<u8>> = r.get("spent_txid")?;
        let spent_height: Option<u32> = r.get("spent_height")?;
        Ok(NoteRecord {
            note: ScannedNote {
                app: b32(r.get("app")?),
                cmx: b32(r.get("cmx")?),
                nullifier: b32(r.get("nullifier")?),
                value: r.get::<_, i64>("value")? as u64,
                memo: r.get("memo")?,
                receiver: r.get("receiver")?,
                receiver_index: r.get::<_, String>("receiver_index")?.parse().unwrap_or(0),
                scope: if r.get::<_, String>("scope")? == "internal" { Scope::Internal } else { Scope::External },
                height: r.get("height")?,
                block_hash: b32(r.get("block_hash")?),
                txid: b32(r.get("txid")?),
            },
            kind: NoteKind::parse(&r.get::<_, String>("kind")?),
            deposit_id: b32(r.get("deposit_id")?),
            state: NoteState::parse(&r.get::<_, String>("state")?),
            decided_memo: r.get("decided_memo")?,
            digest: r.get::<_, Option<Vec<u8>>>("digest")?.map(b32),
            credited_depth: r.get("credited_depth")?,
            rolled_back: r.get::<_, i64>("rolled_back")? != 0,
            spent: spent_txid.zip(spent_height).map(|(t, h)| (b32(t), h)),
        })
    }

    pub fn notes(&self, app: &AppId) -> Result<Vec<NoteRecord>> {
        let mut st = self.db.prepare("SELECT * FROM notes WHERE app=?1 ORDER BY height, cmx")?;
        let rows = st.query_map(params![&app[..]], Self::row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn note_by_deposit_id(&self, id: &[u8; 32]) -> Result<Option<NoteRecord>> {
        Ok(self.db.query_row("SELECT * FROM notes WHERE deposit_id=?1", params![&id[..]], Self::row).optional()?)
    }

    pub fn set_decision(
        &self,
        app: &AppId,
        cmx: &[u8; 32],
        state: NoteState,
        decided_memo: Option<&[u8]>,
        digest: Option<&[u8; 32]>,
        credited_depth: Option<u32>,
    ) -> Result<()> {
        self.db.execute(
            "UPDATE notes SET state=?3, decided_memo=?4, digest=?5, credited_depth=?6 WHERE app=?1 AND cmx=?2",
            params![&app[..], &cmx[..], state.as_str(), decided_memo, digest.map(|d| d.to_vec()), credited_depth],
        )?;
        Ok(())
    }

    pub fn mark_spent(&self, app: &AppId, nullifier: &[u8; 32], txid: &[u8; 32], height: u32) -> Result<bool> {
        let n = self.db.execute(
            "UPDATE notes SET spent_txid=?3, spent_height=?4 WHERE app=?1 AND nullifier=?2",
            params![&app[..], &nullifier[..], &txid[..], height],
        )?;
        Ok(n > 0)
    }

    /// Applies a reorg: notes above `to_height` with no signed decision are
    /// forgotten; decided ones are kept and flagged rolled back. Returns the
    /// decided notes that were rolled back by this call.
    pub fn rewind(&self, to_height: u32) -> Result<Vec<NoteRecord>> {
        let mut st = self.db.prepare(
            "SELECT * FROM notes WHERE height > ?1 AND rolled_back = 0 AND state IN ('credited','voided')",
        )?;
        let rolled: Vec<NoteRecord> = st.query_map(params![to_height], Self::row)?.collect::<rusqlite::Result<_>>()?;
        self.db.execute("UPDATE notes SET rolled_back=1 WHERE height > ?1 AND state IN ('credited','voided')", params![to_height])?;
        self.db.execute("DELETE FROM notes WHERE height > ?1 AND state NOT IN ('credited','voided')", params![to_height])?;
        self.db.execute("UPDATE notes SET spent_txid=NULL, spent_height=NULL WHERE spent_height > ?1", params![to_height])?;
        self.db.execute("DELETE FROM blocks WHERE height > ?1", params![to_height])?;
        Ok(rolled)
    }

    // ------------------------------------------------------------ attestor decision log

    /// Records that this operator's attestor key is about to sign `kind` over
    /// `digest` for `key` (a deposit ID, or a paid sequence key). Idempotent for
    /// the same statement; refuses anything else for the same key. Written with
    /// `synchronous=FULL` before any signature share leaves the process.
    pub fn record_decision(&self, key: &[u8], kind: &str, digest: &[u8; 32]) -> Result<()> {
        let existing: Option<(String, Vec<u8>)> = self
            .db
            .query_row("SELECT kind, digest FROM decisions WHERE key=?1", params![key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match existing {
            Some((k, d)) if k == kind && d == digest => Ok(()),
            Some(_) => Err(StoreError::Conflict),
            None => {
                self.db.execute("INSERT INTO decisions (key,kind,digest) VALUES (?1,?2,?3)", params![key, kind, &digest[..]])?;
                Ok(())
            }
        }
    }

    pub fn decision(&self, key: &[u8]) -> Result<Option<(String, [u8; 32])>> {
        Ok(self
            .db
            .query_row("SELECT kind, digest FROM decisions WHERE key=?1", params![key], |r| {
                Ok((r.get::<_, String>(0)?, b32(r.get(1)?)))
            })
            .optional()?)
    }

    // ------------------------------------------------------------ intents

    /// A deposit memo registered off-chain by an app page, keyed by the session
    /// receiver it will pay. Used only when the note arrives without its memo.
    pub fn put_intent(&self, app: &AppId, receiver_index: u128, session_key: &[u8; 32], memo: &[u8; 512]) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO intents (app,receiver_index,session_key,memo) VALUES (?1,?2,?3,?4)",
            params![&app[..], receiver_index.to_string(), &session_key[..], &memo[..]],
        )?;
        Ok(())
    }

    pub fn intent(&self, app: &AppId, receiver_index: u128) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .query_row(
                "SELECT memo FROM intents WHERE app=?1 AND receiver_index=?2",
                params![&app[..], receiver_index.to_string()],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ------------------------------------------------------------ blocks, halt

    pub fn put_block(&self, height: u32, hash: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let prev = self.block(height)?;
        self.db.execute("INSERT OR REPLACE INTO blocks (height,hash) VALUES (?1,?2)", params![height, &hash[..]])?;
        Ok(prev)
    }

    pub fn block(&self, height: u32) -> Result<Option<[u8; 32]>> {
        Ok(self
            .db
            .query_row("SELECT hash FROM blocks WHERE height=?1", params![height], |r| r.get::<_, Vec<u8>>(0))
            .optional()?
            .map(b32))
    }

    pub fn tip(&self) -> Result<Option<u32>> {
        Ok(self.db.query_row("SELECT MAX(height) FROM blocks", [], |r| r.get(0))?)
    }

    pub fn set_meta(&self, k: &str, v: &[u8]) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO meta (k,v) VALUES (?1,?2)", params![k, v])?;
        Ok(())
    }

    pub fn meta(&self, k: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.db.query_row("SELECT v FROM meta WHERE k=?1", params![k], |r| r.get(0)).optional()?)
    }

    pub fn del_meta(&self, k: &str) -> Result<()> {
        self.db.execute("DELETE FROM meta WHERE k=?1", params![k])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn note(cmx: u8, height: u32) -> ScannedNote {
        ScannedNote {
            app: [1; 32],
            cmx: [cmx; 32],
            nullifier: [cmx ^ 0xFF; 32],
            value: 1_000_000,
            memo: vec![0; 512],
            receiver: vec![2; 43],
            receiver_index: 7,
            scope: Scope::External,
            height,
            block_hash: [height as u8; 32],
            txid: [cmx; 32],
        }
    }

    #[test]
    fn decision_log_refuses_conflicts() {
        let s = Store::in_memory().unwrap();
        s.record_decision(b"dep1", "credit", &[1; 32]).unwrap();
        s.record_decision(b"dep1", "credit", &[1; 32]).unwrap();
        assert!(matches!(s.record_decision(b"dep1", "void", &[2; 32]), Err(StoreError::Conflict)));
        assert!(matches!(s.record_decision(b"dep1", "credit", &[3; 32]), Err(StoreError::Conflict)));
    }

    #[test]
    fn rewind_keeps_decided_notes_and_drops_others() {
        let s = Store::in_memory().unwrap();
        s.insert_note(&note(1, 10), NoteKind::Deposit, &[1; 32]).unwrap();
        s.insert_note(&note(2, 12), NoteKind::Deposit, &[2; 32]).unwrap();
        s.insert_note(&note(3, 12), NoteKind::Deposit, &[3; 32]).unwrap();
        s.set_decision(&[1; 32], &[2; 32], NoteState::Credited, None, Some(&[9; 32]), Some(11)).unwrap();
        s.put_block(12, &[12; 32]).unwrap();
        let rolled = s.rewind(11).unwrap();
        assert_eq!(rolled.len(), 1);
        let notes = s.notes(&[1; 32]).unwrap();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().find(|n| n.note.cmx == [2; 32]).unwrap().rolled_back);
        assert!(s.block(12).unwrap().is_none());
        // The note comes back in the replacement chain.
        s.insert_note(&note(2, 13), NoteKind::Deposit, &[2; 32]).unwrap();
        let n = s.note_by_deposit_id(&[2; 32]).unwrap().unwrap();
        assert!(!n.rolled_back && n.state == NoteState::Credited && n.note.height == 13);
    }

    #[test]
    fn spends_and_intents() {
        let s = Store::in_memory().unwrap();
        s.insert_note(&note(1, 10), NoteKind::Deposit, &[1; 32]).unwrap();
        assert!(s.mark_spent(&[1; 32], &[0xFE; 32], &[7; 32], 20).unwrap());
        assert_eq!(s.notes(&[1; 32]).unwrap()[0].spent, Some(([7; 32], 20)));
        s.rewind(15).unwrap();
        assert_eq!(s.notes(&[1; 32]).unwrap()[0].spent, None);
        s.put_intent(&[1; 32], 99, &[5; 32], &[3; 512]).unwrap();
        assert_eq!(s.intent(&[1; 32], 99).unwrap().unwrap()[0], 3);
        assert!(s.intent(&[1; 32], 98).unwrap().is_none());
    }
}
