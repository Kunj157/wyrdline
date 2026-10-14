//! Apply a checkpoint's changes to the real workspace.

use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde::Serialize;
use wyrd_cas::{Cas, EntryKind, Manifest};

use crate::baseline::{Baseline, Stat};
use crate::{CellError, Result, io_err};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "path")]
pub enum Op {
    Write(String),
    Mkdir(String),
    Symlink(String),
    Delete(String),
    ReplaceDir(String),
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ApplyReport {
    pub ops: Vec<Op>,
    pub conflicts: Vec<String>,
    pub applied: bool,
}

/// Paths the user changed in the real workspace since the baseline,
/// restricted to paths the checkpoint touches (and their contents for
/// deletions and opaque directories).
pub fn conflicts(workspace: &Path, baseline: &Baseline, m: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for e in &m.entries {
        let real = workspace.join(std::ffi::OsStr::from_bytes(&e.path));
        let now = Stat::of(&real);
        let then = baseline.get(&e.path).copied();
        if now != then && !(e.kind == EntryKind::Dir && dir_equivalent(now, then)) {
            out.push(e.path_str());
        }
    }
    out
}

fn dir_equivalent(now: Option<Stat>, then: Option<Stat>) -> bool {
    matches!((now, then), (Some(a), Some(b)) if a.kind == b.kind && a.kind == 0o040_000)
}

/// Plan (and unless `dry_run`, perform) the changes of `m` on `workspace`.
pub fn apply(
    cas: &Cas,
    workspace: &Path,
    baseline: &Baseline,
    m: &Manifest,
    dry_run: bool,
    force: bool,
) -> Result<ApplyReport> {
    let mut report = ApplyReport {
        conflicts: conflicts(workspace, baseline, m),
        ..Default::default()
    };
    for e in &m.entries {
        let p = e.path_str();
        report.ops.push(match e.kind {
            EntryKind::File => Op::Write(p),
            EntryKind::Dir if e.opaque => Op::ReplaceDir(p),
            EntryKind::Dir => Op::Mkdir(p),
            EntryKind::Symlink => Op::Symlink(p),
            EntryKind::Whiteout => Op::Delete(p),
        });
    }
    if dry_run {
        return Ok(report);
    }
    if !report.conflicts.is_empty() && !force {
        return Err(CellError::Conflicts(report.conflicts));
    }
    for e in &m.entries {
        let real = workspace.join(std::ffi::OsStr::from_bytes(&e.path));
        match e.kind {
            EntryKind::Whiteout => remove_any(&real)?,
            EntryKind::Dir => {
                if e.opaque {
                    remove_any(&real)?;
                }
                if !real.is_dir() {
                    remove_any(&real)?;
                    fs::create_dir(&real).map_err(io_err(&real))?;
                }
                fs::set_permissions(&real, fs::Permissions::from_mode(e.mode))
                    .map_err(io_err(&real))?;
            }
            EntryKind::Symlink => {
                remove_any(&real)?;
                let t = e.target.as_ref().map(|t| t.to_vec()).unwrap_or_default();
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&t), &real)
                    .map_err(io_err(&real))?;
            }
            EntryKind::File => {
                if real.is_dir() {
                    remove_any(&real)?;
                }
                let tmp = real.with_file_name(format!(
                    ".{}.wyrd-apply",
                    real.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
                {
                    let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
                    for c in &e.chunks {
                        f.write_all(&cas.get(*c)?).map_err(io_err(&tmp))?;
                    }
                    f.sync_all().map_err(io_err(&tmp))?;
                }
                fs::set_permissions(&tmp, fs::Permissions::from_mode(e.mode))
                    .map_err(io_err(&tmp))?;
                fs::rename(&tmp, &real).map_err(io_err(&real))?;
            }
        }
    }
    report.applied = true;
    Ok(report)
}

fn remove_any(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(p)(e)),
        Ok(m) if m.is_dir() => fs::remove_dir_all(p).map_err(io_err(p)),
        Ok(_) => fs::remove_file(p).map_err(io_err(p)),
    }
}
