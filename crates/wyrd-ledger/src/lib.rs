//! Append-only, hash-chained, signed session ledger (format `wyrdline.ledger.v1`).
//!
//! One SQLite file per session. The `events` table is the chain; every
//! other table is a projection that can be rebuilt with [`Ledger::reindex`].
//! See `docs/ledger-format.md` for the exact on-disk format.

mod chain;
mod key;
mod projections;
mod verify;
mod writer;

pub use chain::{GENESIS, HASH_DOMAIN, Hash, chain_hash};
pub use key::{HostKey, seal_message};
pub use verify::{Problem, Verification, verify};
pub use writer::{WriterHandle, spawn_writer};

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use wyrd_schema::{Event, SCHEMA_ID, to_canonical_cbor};

/// On-disk format identifier stored in the `meta` table.
pub const FORMAT_ID: &str = "wyrdline.ledger.v1";

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("codec: {0}")]
    Codec(#[from] wyrd_schema::CodecError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ledger is sealed; no further events can be appended")]
    Sealed,
    #[error("not a wyrdline ledger or unsupported format: {0}")]
    Format(String),
    #[error("key: {0}")]
    Key(String),
    #[error("writer task stopped")]
    WriterGone,
    #[error("an earlier batch failed to commit: {0}")]
    Commit(String),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

/// Metadata of the seal written when a session closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seal {
    pub head_seq: u64,
    pub head_hash: Hash,
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
    pub sealed_at_ms: i64,
}

/// An open session ledger.
#[derive(Debug)]
pub struct Ledger {
    conn: Connection,
    path: PathBuf,
    session_id: String,
    head_seq: u64,
    head_hash: Hash,
    sealed: bool,
}

const SCHEMA_SQL: &str = r"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE events (
    seq         INTEGER PRIMARY KEY,
    kind        TEXT    NOT NULL,
    plane       TEXT    NOT NULL,
    ts_mono_ns  INTEGER NOT NULL,
    pid         INTEGER,
    event_cbor  BLOB    NOT NULL,
    prev_hash   BLOB    NOT NULL,
    hash        BLOB    NOT NULL
) STRICT;

CREATE TABLE seal (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    head_seq     INTEGER NOT NULL,
    head_hash    BLOB    NOT NULL,
    public_key   BLOB    NOT NULL,
    signature    BLOB    NOT NULL,
    sealed_at_ms INTEGER NOT NULL
) STRICT;

CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER events_no_insert_after_seal BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM seal)
BEGIN SELECT RAISE(ABORT, 'ledger is sealed'); END;

CREATE TRIGGER seal_no_update BEFORE UPDATE ON seal
BEGIN SELECT RAISE(ABORT, 'seal is immutable'); END;

CREATE TRIGGER seal_no_delete BEFORE DELETE ON seal
BEGIN SELECT RAISE(ABORT, 'seal is immutable'); END;
";

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Larger cache and fewer WAL checkpoints keep batched ingest fast.
    conn.pragma_update(None, "cache_size", -32_768)?;
    conn.pragma_update(None, "wal_autocheckpoint", 10_000)?;
    Ok(())
}

impl Ledger {
    /// Create a new ledger file for `session_id`. Fails if the file exists.
    pub fn create(path: impl AsRef<Path>, session_id: &str) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(LedgerError::Format(format!(
                "{} already exists",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA_SQL)?;
        conn.execute_batch(projections::SCHEMA_SQL)?;
        let tx = conn.unchecked_transaction()?;
        for (k, v) in [
            ("format", FORMAT_ID),
            ("event_schema", SCHEMA_ID),
            ("session_id", session_id),
        ] {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                params![k, v],
            )?;
        }
        tx.commit()?;
        Ok(Ledger {
            conn,
            path,
            session_id: session_id.to_string(),
            head_seq: 0,
            head_hash: GENESIS,
            sealed: false,
        })
    }

    /// Open an existing ledger for appending (or reading).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        configure(&conn)?;
        Self::from_conn(conn, path)
    }

    /// Open read-only (used by `verify`).
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Self::from_conn(conn, path)
    }

    fn from_conn(conn: Connection, path: PathBuf) -> Result<Self> {
        let meta = |k: &str| -> Result<Option<String>> {
            Ok(conn
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .optional()?)
        };
        let format = meta("format").map_err(|e| LedgerError::Format(e.to_string()))?;
        if format.as_deref() != Some(FORMAT_ID) {
            return Err(LedgerError::Format(
                format.unwrap_or_else(|| "missing".into()),
            ));
        }
        let session_id = meta("session_id")?.unwrap_or_default();
        let head: Option<(i64, Vec<u8>)> = conn
            .query_row(
                "SELECT seq, hash FROM events ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (head_seq, head_hash) = match head {
            Some((seq, hash)) => (
                u64::try_from(seq).map_err(|_| LedgerError::Format("negative seq".into()))?,
                chain::hash_from_slice(&hash)
                    .ok_or_else(|| LedgerError::Format("bad hash length".into()))?,
            ),
            None => (0, GENESIS),
        };
        let sealed: bool =
            conn.query_row("SELECT EXISTS (SELECT 1 FROM seal)", [], |r| r.get(0))?;
        Ok(Ledger {
            conn,
            path,
            session_id,
            head_seq,
            head_hash,
            sealed,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn head(&self) -> (u64, Hash) {
        (self.head_seq, self.head_hash)
    }

    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Append a batch in one transaction. Each event's `seq` is assigned by
    /// the ledger (any value passed in is overwritten). Returns the sequence
    /// number of the last event.
    pub fn append(&mut self, events: Vec<Event>) -> Result<u64> {
        if self.sealed {
            return Err(LedgerError::Sealed);
        }
        let tx = self.conn.transaction()?;
        let mut seq = self.head_seq;
        let mut prev = self.head_hash;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO events (seq, kind, plane, ts_mono_ns, pid, event_cbor, prev_hash, hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for mut ev in events {
                seq += 1;
                ev.seq = seq;
                let cbor = to_canonical_cbor(&ev)?;
                let hash = chain_hash(&prev, &cbor);
                insert.execute(params![
                    to_i64(seq),
                    ev.kind(),
                    ev.plane().as_str(),
                    to_i64(ev.ts_mono_ns),
                    ev.process.map(|p| p.pid),
                    cbor,
                    prev.as_slice(),
                    hash.as_slice(),
                ])?;
                projections::apply(&tx, &ev)?;
                prev = hash;
            }
        }
        tx.commit()?;
        self.head_seq = seq;
        self.head_hash = prev;
        Ok(seq)
    }

    /// Seal the ledger: sign the head with the host key. No appends afterwards.
    pub fn seal(&mut self, key: &HostKey, sealed_at_ms: i64) -> Result<Seal> {
        if self.sealed {
            return Err(LedgerError::Sealed);
        }
        let msg = seal_message(&self.session_id, self.head_seq, &self.head_hash);
        let seal = Seal {
            head_seq: self.head_seq,
            head_hash: self.head_hash,
            public_key: key.public_key(),
            signature: key.sign(&msg),
            sealed_at_ms,
        };
        self.conn.execute(
            "INSERT INTO seal (id, head_seq, head_hash, public_key, signature, sealed_at_ms)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![
                to_i64(seal.head_seq),
                seal.head_hash.as_slice(),
                seal.public_key.as_slice(),
                seal.signature.as_slice(),
                seal.sealed_at_ms
            ],
        )?;
        self.sealed = true;
        Ok(seal)
    }

    /// Read the seal, if the ledger is sealed.
    pub fn read_seal(&self) -> Result<Option<Seal>> {
        type SealRow = (i64, Vec<u8>, Vec<u8>, Vec<u8>, i64);
        let row: Option<SealRow> = self
            .conn
            .query_row(
                "SELECT head_seq, head_hash, public_key, signature, sealed_at_ms FROM seal",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((seq, hash, pk, sig, at)) = row else {
            return Ok(None);
        };
        let bad = || LedgerError::Format("malformed seal".into());
        Ok(Some(Seal {
            head_seq: u64::try_from(seq).map_err(|_| bad())?,
            head_hash: chain::hash_from_slice(&hash).ok_or_else(bad)?,
            public_key: pk.try_into().map_err(|_| bad())?,
            signature: sig.try_into().map_err(|_| bad())?,
            sealed_at_ms: at,
        }))
    }

    /// Events with `seq > after_seq`, in order, at most `limit`.
    pub fn events_after(&self, after_seq: u64, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT event_cbor FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
        let rows = stmt.query_map(params![to_i64(after_seq), to_i64(limit as u64)], |r| {
            r.get::<_, Vec<u8>>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(wyrd_schema::from_canonical_cbor(&row?)?);
        }
        Ok(out)
    }

    /// Drop and rebuild every projection from the event chain.
    pub fn reindex(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        projections::clear(&tx)?;
        {
            let mut stmt = tx.prepare("SELECT event_cbor FROM events ORDER BY seq")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let ev = wyrd_schema::from_canonical_cbor(&row.get::<_, Vec<u8>>(0)?)?;
                projections::apply(&tx, &ev)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Access to the underlying connection for projection queries.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// SQLite stores INTEGER as i64; sequence numbers and timestamps fit, and
/// values beyond `i64::MAX` saturate rather than wrap.
fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}
