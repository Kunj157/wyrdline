//! Content-addressed chunk store and directory manifests (`wyrdline.cas.v1`).
//!
//! Files are split with `FastCDC` (16/64/256 KiB), each chunk is addressed by
//! the BLAKE3 hash of its uncompressed bytes and stored zstd-compressed at
//! `chunks/ab/cd/<hex>`. A [`Manifest`] describes a directory tree and is
//! itself stored as a chunk, so a checkpoint is just a manifest hash.
//! See `docs/cas-format.md`.

//!
//! Without the default `store` feature only the formats are built
//! ([`ChunkHash`], [`Manifest`]): that is what the Studio's WebAssembly
//! bundle reader uses.
#![forbid(unsafe_code)]

mod manifest;
#[cfg(feature = "store")]
mod snapshot;

pub use manifest::{Entry, EntryKind, MAX_MANIFEST_ENTRIES, Manifest};
#[cfg(feature = "store")]
pub use snapshot::{MaterializeReport, SnapshotStats};

#[cfg(feature = "store")]
use std::collections::HashSet;
use std::fmt;
#[cfg(feature = "store")]
use std::fs;
#[cfg(feature = "store")]
use std::io::{Read, Write as _};
#[cfg(feature = "store")]
use std::path::Path;
use std::path::PathBuf;

/// Chunking parameters.
pub const MIN_CHUNK: usize = 16 * 1024;
pub const AVG_CHUNK: usize = 64 * 1024;
pub const MAX_CHUNK: usize = 256 * 1024;

/// Distinguishes staging files of concurrent writes within one process.
#[cfg(feature = "store")]
static STAGING_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Format identifier written to `<root>/FORMAT`.
pub const FORMAT_ID: &str = "wyrdline.cas.v1";

#[cfg(feature = "store")]
const ZSTD_LEVEL: i32 = 3;

/// BLAKE3 digest of uncompressed chunk bytes.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ChunkHash(#[serde(with = "serde_bytes")] [u8; 32]);

impl ChunkHash {
    pub fn of(bytes: &[u8]) -> Self {
        ChunkHash(*blake3::hash(bytes).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix("b3:").unwrap_or(s);
        let bytes = hex::decode(s).ok()?;
        Some(ChunkHash(bytes.try_into().ok()?))
    }
}

impl fmt::Display for ChunkHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b3:{}", self.to_hex())
    }
}

impl fmt::Debug for ChunkHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b3:{}", &self.to_hex()[..12])
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CasError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("chunk {0} is missing")]
    Missing(ChunkHash),
    #[error("chunk {0} is corrupt")]
    Corrupt(ChunkHash),
    #[error("chunking failed: {0}")]
    Chunking(String),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("unsafe path in manifest: {0:?}")]
    UnsafePath(String),
    #[error("not a {FORMAT_ID} store: {0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, CasError>;

#[cfg(feature = "store")]
pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> CasError + '_ {
    move |source| CasError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Make a directory's entries durable.
#[cfg(feature = "store")]
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err(dir))
}

/// Remove staging files left by a process that is no longer running. A file
/// whose process id is alive, or is this process's, is kept: it may be a
/// write in progress.
#[cfg(feature = "store")]
fn sweep_staging(tmp: &Path) -> Result<()> {
    let own = std::process::id();
    for path in read_dir(tmp)? {
        let pid = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('.').nth(1))
            .and_then(|p| p.parse::<i32>().ok());
        let Some(pid) = pid else { continue };
        if pid.unsigned_abs() == own {
            continue;
        }
        let alive = match rustix::process::Pid::from_raw(pid) {
            // EPERM: the process exists but belongs to someone else.
            Some(p) => match rustix::process::test_kill_process(p) {
                Ok(()) => true,
                Err(e) => e == rustix::io::Errno::PERM,
            },
            None => false,
        };
        if !alive {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Result of [`Cas::gc`].
#[cfg(feature = "store")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub kept: usize,
    pub deleted: usize,
    pub bytes_freed: u64,
}

/// The chunks of one batch (a checkpoint), written under staging names and
/// not yet visible. [`Cas::commit`] makes them visible. A batch that is dropped
/// before its commit removes its staging files.
#[cfg(feature = "store")]
#[derive(Debug, Default)]
pub struct Staged {
    /// `(staging file, chunk path)` pairs not yet renamed into place.
    pub(crate) files: Vec<(PathBuf, PathBuf)>,
}

#[cfg(feature = "store")]
impl Staged {
    /// Staged files not yet committed.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Take the staged files out of this batch (which then removes nothing).
    pub(crate) fn into_files(mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.files)
    }
}

#[cfg(feature = "store")]
impl Drop for Staged {
    fn drop(&mut self) {
        for (tmp, _) in &self.files {
            let _ = fs::remove_file(tmp);
        }
    }
}

/// A chunk store rooted at a directory.
#[cfg(feature = "store")]
#[derive(Debug, Clone)]
pub struct Cas {
    root: PathBuf,
}

#[cfg(feature = "store")]
impl Cas {
    /// Open or create a store at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let chunks = root.join("chunks");
        fs::create_dir_all(&chunks).map_err(io_err(&chunks))?;
        let tmp = root.join("tmp");
        fs::create_dir_all(&tmp).map_err(io_err(&tmp))?;
        sweep_staging(&tmp)?;
        let marker = root.join("FORMAT");
        match fs::read_to_string(&marker) {
            Ok(s) if s.trim() == FORMAT_ID => {}
            Ok(other) => return Err(CasError::Format(other)),
            Err(_) => fs::write(&marker, format!("{FORMAT_ID}\n")).map_err(io_err(&marker))?,
        }
        Ok(Cas { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn chunk_path(&self, h: ChunkHash) -> PathBuf {
        let hex = h.to_hex();
        self.root
            .join("chunks")
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(&hex)
    }

    pub fn contains(&self, h: ChunkHash) -> bool {
        self.chunk_path(h).exists()
    }

    /// Store bytes durably, on their own: when this returns, the chunk and its
    /// directory entry are on disk. An intact chunk that already exists is
    /// not written again; a corrupt one is replaced.
    pub fn put(&self, bytes: &[u8]) -> Result<ChunkHash> {
        let h = ChunkHash::of(bytes);
        let dir = self.chunk_dir(h);
        if self.verified(h)? {
            // The name may have been renamed in by a batch whose directory
            // entry is not durable yet; syncing the directory covers it.
            sync_dir(&dir)?;
            return Ok(h);
        }
        let path = self.chunk_path(h);
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL).map_err(io_err(&path))?;
        let tmp = self.staging_path(h);
        {
            let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
            f.write_all(&compressed).map_err(io_err(&tmp))?;
            f.sync_data().map_err(io_err(&tmp))?;
        }
        fs::rename(&tmp, &path).map_err(io_err(&path))?;
        sync_dir(&dir)?;
        Ok(h)
    }

    /// Stage bytes for a batch. The chunk is written under a unique staging
    /// name and is not visible at its chunk name until [`Cas::commit`]. An
    /// intact chunk that already exists is not staged again.
    pub fn stage(&self, bytes: &[u8], staged: &mut Staged) -> Result<ChunkHash> {
        let h = ChunkHash::of(bytes);
        if self.verified(h)? {
            return Ok(h);
        }
        let path = self.chunk_path(h);
        let dir = self.chunk_dir(h);
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL).map_err(io_err(&path))?;
        let tmp = self.staging_path(h);
        // Registered before the write, so a failed write is removed with the batch.
        staged.files.push((tmp.clone(), path));
        let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(&compressed).map_err(io_err(&tmp))?;
        Ok(h)
    }

    /// Make a batch durable and visible, in this order:
    ///
    /// 1. one barrier ([`Cas::sync`]), so every staged file's data is on disk;
    /// 2. each staged file renamed to its chunk name;
    /// 3. a second barrier, so those names are on disk.
    ///
    /// Only after this may a ledger event name the batch's chunks or manifest.
    /// Both barriers run even for an empty batch: a chunk the batch found
    /// already present may still be waiting for another batch's second barrier.
    pub fn commit(&self, mut staged: Staged) -> Result<()> {
        self.sync()?;
        for (tmp, path) in staged.files.drain(..) {
            fs::rename(&tmp, &path).map_err(io_err(&path))?;
        }
        self.sync()
    }

    /// Whether chunk `h` is present and intact. A chunk that is missing or
    /// wrong is not an error: the caller writes it again.
    fn verified(&self, h: ChunkHash) -> Result<bool> {
        match self.get(h) {
            Ok(_) => Ok(true),
            Err(CasError::Missing(_) | CasError::Corrupt(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn chunk_dir(&self, h: ChunkHash) -> PathBuf {
        let hex = h.to_hex();
        self.root.join("chunks").join(&hex[..2]).join(&hex[2..4])
    }

    /// A staging name, unique per write, so no two writers share a file. The
    /// process id lets [`sweep_staging`] tell a leftover from a live write.
    fn staging_path(&self, h: ChunkHash) -> PathBuf {
        let seq = STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.root
            .join("tmp")
            .join(format!("{}.{}.{seq}", h.to_hex(), std::process::id()))
    }

    /// Make every write so far durable: one barrier for the whole store
    /// filesystem (`syncfs`), covering data, renames and new directories.
    pub fn sync(&self) -> Result<()> {
        let dir = fs::File::open(&self.root).map_err(io_err(&self.root))?;
        rustix::fs::syncfs(&dir).map_err(|e| CasError::Io {
            path: self.root.clone(),
            source: e.into(),
        })
    }

    /// Read a chunk and verify its hash.
    pub fn get(&self, h: ChunkHash) -> Result<Vec<u8>> {
        let path = self.chunk_path(h);
        let compressed = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CasError::Missing(h)),
            Err(e) => return Err(io_err(&path)(e)),
        };
        let bytes = zstd::stream::decode_all(&compressed[..]).map_err(|_| CasError::Corrupt(h))?;
        if ChunkHash::of(&bytes) != h {
            return Err(CasError::Corrupt(h));
        }
        Ok(bytes)
    }

    /// Chunk a stream and store every chunk durably. Returns the chunk list and total size.
    pub fn put_stream(&self, reader: impl Read) -> Result<(Vec<ChunkHash>, u64)> {
        let mut chunks = Vec::new();
        let mut size = 0u64;
        for chunk in fastcdc::v2020::StreamCDC::new(reader, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK) {
            let chunk = chunk.map_err(|e| CasError::Chunking(e.to_string()))?;
            size += chunk.length as u64;
            chunks.push(self.put(&chunk.data)?);
        }
        Ok((chunks, size))
    }

    /// Store a manifest durably; returns its hash.
    pub fn put_manifest(&self, m: &Manifest) -> Result<ChunkHash> {
        self.put(&m.to_cbor()?)
    }

    /// Stage a manifest for a batch; returns its hash.
    pub fn stage_manifest(&self, m: &Manifest, staged: &mut Staged) -> Result<ChunkHash> {
        self.stage(&m.to_cbor()?, staged)
    }

    /// Load and validate a manifest.
    pub fn get_manifest(&self, h: ChunkHash) -> Result<Manifest> {
        Manifest::from_cbor(&self.get(h)?)
    }

    /// Delete every chunk not reachable from `roots` (manifest hashes).
    /// With `dry_run`, only report what would be deleted.
    pub fn gc(&self, roots: &[ChunkHash], dry_run: bool) -> Result<GcReport> {
        let mut live: HashSet<ChunkHash> = HashSet::new();
        for root in roots {
            live.insert(*root);
            for e in self.get_manifest(*root)?.entries {
                live.extend(e.chunks);
            }
        }
        let mut report = GcReport::default();
        let chunks = self.root.join("chunks");
        for l1 in read_dir(&chunks)? {
            for l2 in read_dir(&l1)? {
                for file in read_dir(&l2)? {
                    let name = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    match ChunkHash::from_hex(name) {
                        Some(h) if live.contains(&h) => report.kept += 1,
                        _ => {
                            report.deleted += 1;
                            report.bytes_freed += fs::metadata(&file).map_or(0, |m| m.len());
                            if !dry_run {
                                fs::remove_file(&file).map_err(io_err(&file))?;
                            }
                        }
                    }
                }
            }
        }
        Ok(report)
    }
}

#[cfg(feature = "store")]
fn read_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).map_err(io_err(dir))? {
        out.push(e.map_err(io_err(dir))?.path());
    }
    Ok(out)
}
