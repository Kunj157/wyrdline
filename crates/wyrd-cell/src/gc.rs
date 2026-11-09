//! Garbage collection of the content store (`wyrd gc`). Collection takes the
//! store lock exclusive, computes the roots, and only then sweeps, so a
//! checkpoint or import that is writing chunks at the same moment is never
//! swept from under its own reference (#296; see `docs/cas-format.md`).
//!
//! The roots are every checkpoint manifest of every session in the data
//! directory, from the session record *and* from the ledger's
//! `state.checkpoint.created` events, so a checkpoint known to only one of
//! them still keeps its chunks. A session whose record or ledger cannot be
//! read stops the report: its roots would be missing, and its chunks would
//! look collectable. Two kinds of directory are skipped: an import's staging
//! directory (`.import-<id>`, not a session yet) and a session imported
//! without content (`--no-content`), which names manifests the store never
//! received.

use std::collections::BTreeSet;
use std::fs;
use std::time::Duration;

use wyrd_cas::{Cas, ChunkHash, GcReport};
use wyrd_schema::{EventBody, SessionState};

use crate::state::SessionRecord;
use crate::{CellError, DataDir, Result, io_err, now_ms, state};

/// Events read per call while scanning a ledger.
const BATCH: usize = 10_000;

/// How long a collection waits for a checkpoint or import to finish before it
/// gives up. A writer holds the store lock for the length of its write.
pub const WAIT: Duration = Duration::from_secs(10);

/// Every manifest a session in `data` refers to.
pub fn roots(data: &DataDir) -> Result<BTreeSet<ChunkHash>> {
    roots_excluding(data, &BTreeSet::new())
}

/// [`roots`], leaving out the sessions named in `excluded` (sessions that are
/// being expired by retention).
pub fn roots_excluding(data: &DataDir, excluded: &BTreeSet<String>) -> Result<BTreeSet<ChunkHash>> {
    let mut out = BTreeSet::new();
    let dir = data.sessions();
    let entries = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(io_err(&dir)(e)),
    };
    for e in entries {
        let e = e.map_err(io_err(&dir))?;
        if !e.file_type().map_err(io_err(&e.path()))?.is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        if id.starts_with(".import-") || excluded.contains(&id) {
            continue;
        }
        let s = data.session(&id);
        if let Ok(bytes) = fs::read(s.import())
            && serde_json::from_slice::<crate::ImportSource>(&bytes).is_ok_and(|src| !src.content)
        {
            continue;
        }
        let rec = state::load(&s).map_err(|err| {
            CellError::Invalid(format!("session {id}: its record cannot be read ({err})"))
        })?;
        for c in &rec.checkpoints {
            out.insert(parse(&id, &c.manifest)?);
        }
        let ledger = wyrd_ledger::Ledger::open_read_only(s.ledger()).map_err(|err| {
            CellError::Invalid(format!("session {id}: its ledger cannot be read ({err})"))
        })?;
        let mut after = 0;
        loop {
            let batch = ledger.events_after(after, BATCH)?;
            let Some(last) = batch.last() else { break };
            after = last.seq;
            for ev in &batch {
                if let EventBody::CheckpointCreated(c) = &ev.body {
                    out.insert(parse(&id, &c.manifest)?);
                }
            }
        }
    }
    Ok(out)
}

fn parse(session: &str, manifest: &str) -> Result<ChunkHash> {
    ChunkHash::from_hex(manifest).ok_or_else(|| {
        CellError::Invalid(format!("session {session}: bad manifest hash {manifest}"))
    })
}

/// Collect the content store: delete every chunk that no session's checkpoint
/// reaches, and the staging files of dead processes. With `dry_run`, delete
/// nothing and only report what would be freed. Waits at most `wait` for a
/// checkpoint or import to finish; fails, deleting nothing, if the store is
/// still busy then, or if a session cannot be read.
pub fn collect(data: &DataDir, dry_run: bool, wait: Duration) -> Result<GcReport> {
    let cas = Cas::open(data.cas())?;
    let lock = cas.lock_exclusive(wait)?;
    let roots: Vec<ChunkHash> = roots(data)?.into_iter().collect();
    Ok(cas.gc(&lock, &roots, dry_run)?)
}

/// What collecting the store now would keep and free. Nothing is deleted.
pub fn dry_run(data: &DataDir) -> Result<GcReport> {
    collect(data, true, WAIT)
}

/// Which sessions retention may expire. Both limits are opt-in: with neither
/// set, nothing expires. A session is kept when it is among the newest
/// `keep_sessions` closed sessions, or was created within `keep_days`; a
/// session expires only when neither applies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retention {
    pub keep_days: Option<u32>,
    pub keep_sessions: Option<usize>,
}

impl Retention {
    pub fn is_set(&self) -> bool {
        self.keep_days.is_some() || self.keep_sessions.is_some()
    }
}

/// What a collection with retention did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collection {
    pub report: GcReport,
    /// Sessions removed by retention (with a dry run: the ones that would be).
    pub expired: Vec<String>,
    /// Sessions retention wanted to expire but that were running or busy.
    pub kept_busy: Vec<String>,
}

/// Directory, inside the data directory but outside `sessions/`, where an
/// expired session is moved before it is removed. Moving it first makes the
/// removal atomic from the point of view of `roots`.
fn trash(data: &DataDir) -> std::path::PathBuf {
    data.root().join(".trash")
}

/// Load every session record. An unreadable one stops the collection, as in
/// `roots`, before anything is changed.
fn records(data: &DataDir) -> Result<Vec<(String, SessionRecord)>> {
    let mut out = Vec::new();
    let dir = data.sessions();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Ok(out);
    };
    for e in rd {
        let e = e.map_err(io_err(&dir))?;
        if !e.file_type().map_err(io_err(&e.path()))?.is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let rec = state::load(&data.session(&id)).map_err(|err| {
            CellError::Invalid(format!("session {id}: its record cannot be read ({err})"))
        })?;
        out.push((id, rec));
    }
    Ok(out)
}

/// The sessions retention expires, from `recs`: closed, not imported, not
/// possibly running, and beyond the limits.
fn due(data: &DataDir, recs: &[(String, SessionRecord)], r: &Retention, now: i64) -> Vec<String> {
    if !r.is_set() {
        return Vec::new();
    }
    let mut closed: Vec<&(String, SessionRecord)> = recs
        .iter()
        .filter(|(id, rec)| {
            rec.status == SessionState::Closed
                && !data.session(id).import().exists()
                && !state::may_be_running(rec)
        })
        .collect();
    closed.sort_by_key(|(_, rec)| std::cmp::Reverse(rec.created_ms));
    closed
        .iter()
        .enumerate()
        .filter(|(i, (_, rec))| {
            let among_newest = r.keep_sessions.is_some_and(|n| *i < n);
            let young = r
                .keep_days
                .is_some_and(|d| now - rec.created_ms <= i64::from(d) * 86_400_000);
            !(among_newest || young)
        })
        .map(|(_, (id, _))| id.clone())
        .collect()
}

/// Collect the content store, expiring sessions by `retention` first. A session
/// that is running or whose lock is held is kept and reported in `kept_busy`;
/// retention never waits for a session lock, since a checkpoint takes the
/// session lock before the store lock (`docs/cas-format.md`). With `dry_run`,
/// nothing is moved or deleted.
pub fn collect_retained(
    data: &DataDir,
    dry_run: bool,
    wait: Duration,
    r: &Retention,
) -> Result<Collection> {
    let cas = Cas::open(data.cas())?;
    let lock = cas.lock_exclusive(wait)?;
    let recs = records(data)?;
    let due_ids = due(data, &recs, r, now_ms());
    let mut expired = Vec::new();
    let mut kept_busy = Vec::new();
    if dry_run {
        for id in due_ids {
            if state::is_locked(&data.session(&id).lock())? {
                kept_busy.push(id);
            } else {
                expired.push(id);
            }
        }
    } else if !due_ids.is_empty() {
        let trash_dir = trash(data);
        fs::create_dir_all(&trash_dir).map_err(io_err(&trash_dir))?;
        for id in due_ids {
            let s = data.session(&id);
            let Some(held) = state::try_lock(&s.lock())? else {
                kept_busy.push(id);
                continue;
            };
            // Re-read under the lock: the state may have changed since the scan.
            let rec = state::load(&s)?;
            if rec.status != SessionState::Closed || state::may_be_running(&rec) {
                kept_busy.push(id);
                continue;
            }
            let moved = trash_dir.join(&id);
            fs::rename(s.root(), &moved).map_err(io_err(&moved))?;
            wyrd_cas::sync_dir(&data.sessions())?;
            drop(held);
            expired.push(id);
        }
        // Best effort: a root-owned work directory can keep the trash in place,
        // and then the removed session's files stay on disk until removed by hand.
        let _ = fs::remove_dir_all(&trash_dir);
    }
    let excluded: BTreeSet<String> = expired.iter().cloned().collect();
    let roots: Vec<ChunkHash> = roots_excluding(data, &excluded)?.into_iter().collect();
    let report = cas.gc(&lock, &roots, dry_run)?;
    Ok(Collection {
        report,
        expired,
        kept_busy,
    })
}
