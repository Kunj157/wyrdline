//! The session record (`session.json`), guarded by an advisory lock.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wyrd_schema::{CaptureMode, SessionState};

use crate::{CellError, Result, SessionDir, io_err};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub id: String,
    pub name: String,
    pub forked_from: Option<String>,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub id: String,
    pub branch: String,
    pub parent: Option<String>,
    /// Manifest hash, `b3:` + hex.
    pub manifest: String,
    pub seq: u64,
    pub created_ms: i64,
    pub files: u64,
    pub bytes_read: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub name: Option<String>,
    pub workspace: PathBuf,
    pub command: Vec<String>,
    pub mode: CaptureMode,
    pub created_ms: i64,
    pub status: SessionState,
    pub current_branch: String,
    pub branches: Vec<Branch>,
    pub checkpoints: Vec<CheckpointRecord>,
    /// cgroup of the running Cell, if any.
    pub cgroup: Option<PathBuf>,
    pub pid: Option<u32>,
    pub runs: u32,
}

impl SessionRecord {
    pub fn branch(&self, id_or_name: &str) -> Option<&Branch> {
        self.branches
            .iter()
            .find(|b| b.id == id_or_name || b.name == id_or_name)
    }

    pub fn checkpoint(&self, id: &str) -> Option<&CheckpointRecord> {
        self.checkpoints.iter().find(|c| c.id == id)
    }

    /// Latest checkpoint of a branch.
    pub fn branch_head(&self, branch: &str) -> Option<&CheckpointRecord> {
        self.checkpoints.iter().rev().find(|c| c.branch == branch)
    }

    pub fn next_checkpoint_id(&self) -> String {
        format!("c{}", self.checkpoints.len() + 1)
    }

    /// Branch ids are `a`, `b`, …, `z`, `aa`, `ab`, …
    pub fn next_branch_id(&self) -> String {
        let mut n = self.branches.len();
        let mut id = String::new();
        loop {
            id.insert(0, char::from(b'a' + u8::try_from(n % 26).unwrap_or(0)));
            n /= 26;
            if n == 0 {
                break;
            }
            n -= 1;
        }
        id
    }
}

pub fn load(dir: &SessionDir) -> Result<SessionRecord> {
    let path = dir.state();
    let bytes = fs::read(&path).map_err(io_err(&path))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| CellError::Invalid(format!("{}: {e}", path.display())))
}

pub fn save(dir: &SessionDir, rec: &SessionRecord) -> Result<()> {
    let path = dir.state();
    let tmp = dir.root().join("session.json.tmp");
    let bytes = serde_json::to_vec_pretty(rec).map_err(|e| CellError::Invalid(e.to_string()))?;
    let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
    f.write_all(&bytes).map_err(io_err(&tmp))?;
    f.sync_all().map_err(io_err(&tmp))?;
    fs::rename(&tmp, &path).map_err(io_err(&path))
}

/// Exclusive advisory lock on the session; released on drop.
#[derive(Debug)]
pub struct SessionLock {
    _file: fs::File,
}

pub fn lock(path: &Path) -> Result<SessionLock> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io_err(path))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).map_err(|e| {
        CellError::Io {
            path: path.to_path_buf(),
            source: e.into(),
        }
    })?;
    Ok(SessionLock { _file: file })
}
