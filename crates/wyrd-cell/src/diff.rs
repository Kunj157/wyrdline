//! Differences between two checkpoints (their upper-layer manifests).

use std::collections::BTreeSet;

use serde::Serialize;
use wyrd_cas::{EntryKind, Manifest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, ts_rs::TS)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    /// What the path is in the newer manifest (or the older one when removed).
    pub entry: &'static str,
}

fn kind_name(k: EntryKind) -> &'static str {
    match k {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Whiteout => "deleted",
    }
}

/// Changes from `a` to `b`. Directory mtime-only changes are ignored.
pub fn diff(a: &Manifest, b: &Manifest) -> Vec<Change> {
    let paths: BTreeSet<&[u8]> = a
        .entries
        .iter()
        .chain(&b.entries)
        .map(|e| e.path.as_slice())
        .collect();
    let mut out = Vec::new();
    for p in paths {
        let path = String::from_utf8_lossy(p).into_owned();
        match (a.get(p), b.get(p)) {
            (None, Some(n)) => out.push(Change {
                path,
                kind: ChangeKind::Added,
                entry: kind_name(n.kind),
            }),
            (Some(o), None) => out.push(Change {
                path,
                kind: ChangeKind::Removed,
                entry: kind_name(o.kind),
            }),
            (Some(o), Some(n)) => {
                let same = o.kind == n.kind
                    && o.mode == n.mode
                    && o.chunks == n.chunks
                    && o.target == n.target
                    && o.opaque == n.opaque
                    && (o.kind == EntryKind::Dir || o.size == n.size);
                if !same {
                    out.push(Change {
                        path,
                        kind: ChangeKind::Modified,
                        entry: kind_name(n.kind),
                    });
                }
            }
            (None, None) => {}
        }
    }
    out
}
