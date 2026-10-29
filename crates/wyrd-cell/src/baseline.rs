//! Stat baseline of the real workspace, taken when a session starts, used
//! by `apply` to detect files the user changed in the meantime.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::{CellError, Result, io_err};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    /// `S_IFMT` bits.
    pub kind: u32,
    pub size: u64,
    pub mtime_ns: i64,
    pub mode: u32,
}

impl Stat {
    pub fn of(path: &Path) -> Option<Stat> {
        let m = fs::symlink_metadata(path).ok()?;
        Some(Stat {
            kind: m.mode() & 0o170_000,
            // Directory sizes change with their contents; compare kind and mode only.
            size: if m.is_dir() { 0 } else { m.len() },
            mtime_ns: if m.is_dir() {
                0
            } else {
                m.mtime() * 1_000_000_000 + m.mtime_nsec()
            },
            mode: m.mode() & 0o7777,
        })
    }
}

/// Relative path bytes → stat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub entries: BTreeMap<ByteBuf, Stat>,
}

impl Baseline {
    /// Walk `root` without following symlinks.
    pub fn capture(root: &Path) -> Result<Baseline> {
        let mut entries = BTreeMap::new();
        let mut stack = vec![(root.to_path_buf(), Vec::<u8>::new())];
        while let Some((dir, rel)) = stack.pop() {
            for e in fs::read_dir(&dir).map_err(io_err(&dir))? {
                let e = e.map_err(io_err(&dir))?;
                let mut r = rel.clone();
                if !r.is_empty() {
                    r.push(b'/');
                }
                r.extend_from_slice(e.file_name().as_bytes());
                let p = e.path();
                let Some(st) = Stat::of(&p) else { continue };
                if st.kind == 0o040_000 {
                    stack.push((p, r.clone()));
                }
                entries.insert(ByteBuf::from(r), st);
            }
        }
        Ok(Baseline { entries })
    }

    pub fn get(&self, rel: &[u8]) -> Option<&Stat> {
        self.entries.get(&ByteBuf::from(rel.to_vec()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut out = Vec::new();
        ciborium::into_writer(self, &mut out).map_err(|e| CellError::Invalid(e.to_string()))?;
        Ok(wyrd_cas::write_durable(path, &out)?)
    }

    pub fn load(path: &Path) -> Result<Baseline> {
        let bytes = fs::read(path).map_err(io_err(path))?;
        ciborium::from_reader(&bytes[..]).map_err(|e| CellError::Invalid(e.to_string()))
    }
}
