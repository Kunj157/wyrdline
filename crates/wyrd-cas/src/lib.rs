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

/// One durable step of a write, in the order it was taken.
#[cfg(feature = "store")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A directory was created (its parent is synced right after).
    Mkdir(PathBuf),
    /// A directory's entries were synced.
    SyncDir(PathBuf),
    /// A file's data was synced.
    SyncFile(PathBuf),
    /// A file was renamed `(from, to)`.
    Rename(PathBuf, PathBuf),
}

#[cfg(feature = "store")]
thread_local! {
    static STEPS: std::cell::RefCell<Option<Vec<Step>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "store")]
fn step(s: Step) {
    STEPS.with(|r| {
        if let Some(v) = r.borrow_mut().as_mut() {
            v.push(s);
        }
    });
}

/// Run `f` on this thread and return its result with the durable steps it
/// took, in order. For tests that pin the order of writes; the recording is
/// off unless this is called.
#[doc(hidden)]
#[cfg(feature = "store")]
pub fn record_steps<R>(f: impl FnOnce() -> R) -> (R, Vec<Step>) {
    STEPS.with(|r| *r.borrow_mut() = Some(Vec::new()));
    let out = f();
    let steps = STEPS.with(|r| r.borrow_mut().take().unwrap_or_default());
    (out, steps)
}

/// Sync a directory's entries.
#[cfg(feature = "store")]
fn sync_dir(dir: &Path) -> Result<()> {
    step(Step::SyncDir(dir.to_path_buf()));
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err(dir))
}

/// Create `dir` and any missing parents. A new directory is an entry in its
/// parent, so the parent is synced straight after the directory is made,
/// before anything is written under it. A directory another process created
/// first is synced the same way, because that process may not have synced it.
#[cfg(feature = "store")]
fn create_dirs_durable(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        create_dirs_durable(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {
            step(Step::Mkdir(dir.to_path_buf()));
            if let Some(parent) = dir.parent() {
                sync_dir(parent)?;
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(parent) = dir.parent() {
                sync_dir(parent)?;
            }
            Ok(())
        }
        Err(e) => Err(io_err(dir)(e)),
    }
}

/// Result of [`Cas::gc`].
#[cfg(feature = "store")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub kept: usize,
    pub deleted: usize,
    pub bytes_freed: u64,
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
        create_dirs_durable(&chunks)?;
        create_dirs_durable(&root.join("tmp"))?;
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

    /// Store bytes; returns their hash. Existing chunks are not rewritten.
    pub fn put(&self, bytes: &[u8]) -> Result<ChunkHash> {
        let h = ChunkHash::of(bytes);
        let path = self.chunk_path(h);
        if path.exists() {
            return Ok(h);
        }
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL).map_err(io_err(&path))?;
        let dir = path.parent().unwrap_or(&self.root);
        create_dirs_durable(dir)?;
        let tmp = self
            .root
            .join("tmp")
            .join(format!("{}.{}", h.to_hex(), std::process::id()));
        {
            let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
            f.write_all(&compressed).map_err(io_err(&tmp))?;
            step(Step::SyncFile(tmp.clone()));
            f.sync_data().map_err(io_err(&tmp))?;
        }
        step(Step::Rename(tmp.clone(), path.clone()));
        fs::rename(&tmp, &path).map_err(io_err(&path))?;
        // The chunk's name is an entry in its directory: sync that after the rename.
        sync_dir(dir)?;
        Ok(h)
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

    /// Chunk a stream and store every chunk. Returns the chunk list and total size.
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

    /// Store a manifest; returns its hash.
    pub fn put_manifest(&self, m: &Manifest) -> Result<ChunkHash> {
        self.put(&m.to_cbor()?)
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
