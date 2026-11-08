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
use wyrd_schema::EventBody;

use crate::{CellError, DataDir, Result, io_err, state};

/// Events read per call while scanning a ledger.
const BATCH: usize = 10_000;

/// How long a collection waits for a checkpoint or import to finish before it
/// gives up. A writer holds the store lock for the length of its write.
pub const WAIT: Duration = Duration::from_secs(10);

/// Every manifest a session in `data` refers to.
pub fn roots(data: &DataDir) -> Result<BTreeSet<ChunkHash>> {
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
        if id.starts_with(".import-") {
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
/// nothing and only report what would be freed. Fails, deleting nothing, if
/// the store is still busy after [`WAIT`] or a session cannot be read.
pub fn collect(data: &DataDir, dry_run: bool) -> Result<GcReport> {
    let cas = Cas::open(data.cas())?;
    let lock = cas.lock_exclusive(WAIT)?;
    let roots: Vec<ChunkHash> = roots(data)?.into_iter().collect();
    Ok(cas.gc(&lock, &roots, dry_run)?)
}

/// What collecting the store now would keep and free. Nothing is deleted.
pub fn dry_run(data: &DataDir) -> Result<GcReport> {
    collect(data, true)
}
