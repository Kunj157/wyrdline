//! Snapshot a directory into a manifest and materialise a manifest.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_bytes::ByteBuf;

use crate::manifest::{Entry, EntryKind, Manifest};
use crate::{Cas, CasError, ChunkHash, Result, Staged, io_err};

/// Statistics of a snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub whiteouts: u64,
    /// Files whose content was re-read (not reused from the base manifest).
    pub files_read: u64,
    pub bytes_read: u64,
    /// FIFOs, sockets, and devices other than whiteouts are not captured.
    pub skipped_special: u64,
}

/// One file's stored chunks and size, or why it could not be stored.
type Stored = (usize, Result<(Vec<ChunkHash>, u64)>);

/// What [`Cas::materialize`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializeReport {
    pub files: u64,
    pub bytes: u64,
    /// Whiteouts the caller must recreate (needs overlay-aware handling).
    pub whiteouts: Vec<PathBuf>,
    /// Directories that must be marked opaque by the caller.
    pub opaque_dirs: Vec<PathBuf>,
}

const OPAQUE_XATTRS: [&str; 2] = ["trusted.overlay.opaque", "user.overlay.opaque"];

fn is_opaque(path: &Path) -> bool {
    let mut buf = [0u8; 8];
    OPAQUE_XATTRS.iter().any(|name| {
        rustix::fs::lgetxattr(path, *name, &mut buf[..]).is_ok_and(|n| n >= 1 && buf[0] == b'y')
    })
}

impl Cas {
    /// Snapshot `dir` and store every changed file durably. When `base` is
    /// given, files whose size, mtime, and mode are unchanged reuse the
    /// base's chunk list without being read.
    pub fn snapshot(
        &self,
        dir: &Path,
        base: Option<&Manifest>,
    ) -> Result<(Manifest, SnapshotStats)> {
        let mut staged = Staged::default();
        let snap = self.snapshot_staged(dir, base, &mut staged)?;
        self.commit(staged)?;
        Ok(snap)
    }

    /// As [`Cas::snapshot`], but the changed files are only staged, into
    /// `staged`. Nothing is visible until [`Cas::commit`].
    pub fn snapshot_staged(
        &self,
        dir: &Path,
        base: Option<&Manifest>,
        staged: &mut Staged,
    ) -> Result<(Manifest, SnapshotStats)> {
        let base_index: HashMap<&[u8], &Entry> = base
            .map(|m| m.entries.iter().map(|e| (e.path.as_slice(), e)).collect())
            .unwrap_or_default();
        let mut entries = Vec::new();
        let mut stats = SnapshotStats::default();
        // Changed files, read after the walk: each is chunked and stored by
        // one of several workers, so a checkpoint is not one file at a time.
        let mut pending: Vec<(usize, PathBuf)> = Vec::new();
        let mut stack: Vec<(PathBuf, Vec<u8>)> = vec![(dir.to_path_buf(), Vec::new())];
        while let Some((abs, rel)) = stack.pop() {
            let mut children: Vec<_> = fs::read_dir(&abs)
                .map_err(io_err(&abs))?
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(io_err(&abs))?;
            children.sort_by_key(fs::DirEntry::file_name);
            for child in children {
                let path = child.path();
                let name = child.file_name();
                let mut rel_path = rel.clone();
                if !rel_path.is_empty() {
                    rel_path.push(b'/');
                }
                rel_path.extend_from_slice(name.as_bytes());
                let meta = fs::symlink_metadata(&path).map_err(io_err(&path))?;
                let ft = meta.file_type();
                let mode = meta.mode() & 0o7777;
                let mtime_ns = meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
                let mut entry = Entry {
                    path: ByteBuf::from(rel_path.clone()),
                    kind: EntryKind::File,
                    mode,
                    size: 0,
                    mtime_ns,
                    chunks: Vec::new(),
                    target: None,
                    opaque: false,
                };
                if ft.is_dir() {
                    entry.kind = EntryKind::Dir;
                    entry.opaque = is_opaque(&path);
                    stats.dirs += 1;
                    stack.push((path, rel_path));
                } else if ft.is_symlink() {
                    entry.kind = EntryKind::Symlink;
                    let target = fs::read_link(&path).map_err(io_err(&path))?;
                    entry.target = Some(ByteBuf::from(target.as_os_str().as_bytes().to_vec()));
                    stats.symlinks += 1;
                } else if ft.is_char_device() && meta.rdev() == 0 {
                    entry.kind = EntryKind::Whiteout;
                    entry.mode = 0;
                    entry.mtime_ns = 0;
                    stats.whiteouts += 1;
                } else if ft.is_file() {
                    stats.files += 1;
                    let reusable = base_index.get(rel_path.as_slice()).filter(|b| {
                        b.kind == EntryKind::File
                            && b.size == meta.len()
                            && b.mtime_ns == mtime_ns
                            && b.mode == mode
                            && b.chunks.iter().all(|c| self.contains(*c))
                    });
                    if let Some(b) = reusable {
                        entry.size = b.size;
                        entry.chunks.clone_from(&b.chunks);
                    } else {
                        pending.push((entries.len(), path.clone()));
                    }
                } else {
                    stats.skipped_special += 1;
                    continue;
                }
                entries.push(entry);
            }
        }
        let (stored, batch) = self.stage_files(&pending);
        staged.files.extend(batch.into_files());
        if stored.len() != pending.len() {
            // A worker died without reporting: never build a manifest with a
            // file's chunks missing.
            return Err(CasError::Manifest(
                "a snapshot worker did not finish".into(),
            ));
        }
        for (index, result) in stored {
            let (chunks, size) = result?;
            entries[index].size = size;
            entries[index].chunks = chunks;
            stats.files_read += 1;
            stats.bytes_read += size;
        }
        Ok((Manifest::new(entries)?, stats))
    }

    /// Chunk and stage each file, in parallel. Each worker has its own batch,
    /// merged after the join. The manifest is sorted by path, so it does not
    /// depend on the order in which the workers finish.
    fn stage_files(&self, files: &[(usize, PathBuf)]) -> (Vec<Stored>, Staged) {
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .clamp(1, 8)
            .min(files.len().max(1));
        let next = AtomicUsize::new(0);
        let parts: Vec<(Vec<Stored>, Staged)> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    s.spawn(|| {
                        let mut done = Vec::new();
                        let mut batch = Staged::default();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some((index, path)) = files.get(i) else {
                                break;
                            };
                            let stored = fs::File::open(path)
                                .map_err(io_err(path))
                                .and_then(|f| self.stage_stream(f, &mut batch));
                            done.push((*index, stored));
                        }
                        (done, batch)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect()
        });
        let mut all = Staged::default();
        let mut stored = Vec::new();
        for (done, batch) in parts {
            stored.extend(done);
            all.files.extend(batch.into_files());
        }
        (stored, all)
    }

    /// Chunk a stream and stage every chunk.
    fn stage_stream(
        &self,
        reader: impl std::io::Read,
        staged: &mut Staged,
    ) -> Result<(Vec<ChunkHash>, u64)> {
        let mut chunks = Vec::new();
        let mut size = 0u64;
        for chunk in fastcdc::v2020::StreamCDC::new(
            reader,
            crate::MIN_CHUNK,
            crate::AVG_CHUNK,
            crate::MAX_CHUNK,
        ) {
            let chunk = chunk.map_err(|e| CasError::Chunking(e.to_string()))?;
            size += chunk.length as u64;
            chunks.push(self.stage(&chunk.data, staged)?);
        }
        Ok((chunks, size))
    }

    /// Recreate `manifest` under `dest`, which must not exist or be empty.
    /// Whiteouts and opaque markers are returned for the caller to apply.
    pub fn materialize(&self, manifest: &Manifest, dest: &Path) -> Result<MaterializeReport> {
        manifest.validate()?;
        if dest.exists() {
            let mut it = fs::read_dir(dest).map_err(io_err(dest))?;
            if it.next().is_some() {
                return Err(CasError::UnsafePath(format!(
                    "{} is not empty",
                    dest.display()
                )));
            }
        } else {
            fs::create_dir_all(dest).map_err(io_err(dest))?;
        }
        let mut report = MaterializeReport::default();
        let mut dir_modes: Vec<(PathBuf, u32, i64)> = Vec::new();
        for e in &manifest.entries {
            let path = dest.join(std::ffi::OsStr::from_bytes(&e.path));
            match e.kind {
                EntryKind::Dir => {
                    fs::create_dir(&path).map_err(io_err(&path))?;
                    dir_modes.push((path.clone(), e.mode, e.mtime_ns));
                    if e.opaque {
                        report.opaque_dirs.push(path);
                    }
                }
                EntryKind::File => {
                    let mut f = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(io_err(&path))?;
                    for c in &e.chunks {
                        f.write_all(&self.get(*c)?).map_err(io_err(&path))?;
                    }
                    drop(f);
                    fs::set_permissions(&path, fs::Permissions::from_mode(e.mode))
                        .map_err(io_err(&path))?;
                    set_mtime(&path, e.mtime_ns)?;
                    report.files += 1;
                    report.bytes += e.size;
                }
                EntryKind::Symlink => {
                    let target = e.target.as_ref().map(|t| t.to_vec()).unwrap_or_default();
                    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), &path)
                        .map_err(io_err(&path))?;
                    set_mtime(&path, e.mtime_ns)?;
                }
                EntryKind::Whiteout => report.whiteouts.push(path),
            }
        }
        // Directory modes and times last, deepest first, so writes inside succeed.
        for (path, mode, mtime) in dir_modes.into_iter().rev() {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(io_err(&path))?;
            set_mtime(&path, mtime)?;
        }
        Ok(report)
    }
}

fn set_mtime(path: &Path, mtime_ns: i64) -> Result<()> {
    let ts = rustix::fs::Timespec {
        tv_sec: mtime_ns.div_euclid(1_000_000_000),
        tv_nsec: mtime_ns.rem_euclid(1_000_000_000),
    };
    let times = rustix::fs::Timestamps {
        last_access: ts,
        last_modification: ts,
    };
    rustix::fs::utimensat(
        rustix::fs::CWD,
        path,
        &times,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| CasError::Io {
        path: path.to_path_buf(),
        source: e.into(),
    })
}
