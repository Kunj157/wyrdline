//! Directory manifests.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::{CasError, ChunkHash, Result};

/// Upper bound on entries accepted when decoding a manifest.
pub const MAX_MANIFEST_ENTRIES: usize = 2_000_000;

const MANIFEST_FORMAT: &str = "wyrdline.cas.v1/manifest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// Overlay whiteout (character device 0/0): the path was deleted.
    Whiteout,
}

/// One path in a tree. Paths are relative, `/`-separated raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: ByteBuf,
    pub kind: EntryKind,
    /// Permission bits (`st_mode & 0o7777`).
    pub mode: u32,
    pub size: u64,
    pub mtime_ns: i64,
    pub chunks: Vec<ChunkHash>,
    /// Symlink target.
    pub target: Option<ByteBuf>,
    /// Overlay opaque directory (hides the lower layer's contents).
    pub opaque: bool,
}

impl Entry {
    pub fn path_str(&self) -> String {
        String::from_utf8_lossy(&self.path).into_owned()
    }
}

/// A sorted, validated list of entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub entries: Vec<Entry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            format: MANIFEST_FORMAT.to_string(),
            entries: Vec::new(),
        }
    }
}

impl Manifest {
    /// Build from entries in any order; sorts and validates.
    pub fn new(mut entries: Vec<Entry>) -> Result<Self> {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let m = Manifest {
            format: MANIFEST_FORMAT.to_string(),
            entries,
        };
        m.validate()?;
        Ok(m)
    }

    pub fn to_cbor(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        ciborium::into_writer(self, &mut out).map_err(|e| CasError::Manifest(e.to_string()))?;
        Ok(out)
    }

    pub fn from_cbor(bytes: &[u8]) -> Result<Self> {
        let m: Manifest = ciborium::de::from_reader_with_recursion_limit(bytes, 16)
            .map_err(|e| CasError::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn get(&self, path: &[u8]) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.path.as_slice().cmp(path))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Total bytes of regular files.
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.size)
            .sum()
    }

    /// Checks format, ordering, uniqueness, path safety, and that every
    /// entry's parent is a directory entry (so a symlink can never be used
    /// as a path component during materialisation).
    pub fn validate(&self) -> Result<()> {
        if self.format != MANIFEST_FORMAT {
            return Err(CasError::Manifest(format!(
                "unknown format {:?}",
                self.format
            )));
        }
        if self.entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(CasError::Manifest("too many entries".into()));
        }
        let mut dirs: HashSet<&[u8]> = HashSet::new();
        for (i, e) in self.entries.iter().enumerate() {
            check_path(&e.path)?;
            if i > 0 && self.entries[i - 1].path >= e.path {
                return Err(CasError::Manifest(format!(
                    "entries not sorted/unique at {}",
                    e.path_str()
                )));
            }
            if let Some(pos) = e.path.iter().rposition(|&b| b == b'/') {
                let parent = &e.path[..pos];
                if !dirs.contains(parent) {
                    return Err(CasError::UnsafePath(format!(
                        "{} has no directory entry for its parent",
                        e.path_str()
                    )));
                }
            }
            match e.kind {
                EntryKind::Dir => {
                    dirs.insert(&e.path);
                }
                EntryKind::Symlink if e.target.is_none() => {
                    return Err(CasError::Manifest(format!(
                        "symlink {} without target",
                        e.path_str()
                    )));
                }
                EntryKind::File | EntryKind::Symlink | EntryKind::Whiteout => {}
            }
            if e.kind != EntryKind::File && !e.chunks.is_empty() {
                return Err(CasError::Manifest(format!(
                    "{} is not a file but has chunks",
                    e.path_str()
                )));
            }
        }
        Ok(())
    }
}

fn check_path(p: &[u8]) -> Result<()> {
    let bad = || CasError::UnsafePath(String::from_utf8_lossy(p).into_owned());
    if p.is_empty() || p.len() > 4096 || p[0] == b'/' || p.contains(&0) {
        return Err(bad());
    }
    for comp in p.split(|&b| b == b'/') {
        if comp.is_empty() || comp == b"." || comp == b".." || comp.len() > 255 {
            return Err(bad());
        }
    }
    Ok(())
}
