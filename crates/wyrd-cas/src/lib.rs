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
        fs::create_dir_all(&chunks).map_err(io_err(&chunks))?;
        fs::create_dir_all(root.join("tmp")).map_err(io_err(&root))?;
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

    /// Store bytes durably: when this returns, the chunk and its directory
    /// entry are on disk. Existing chunks are not rewritten. Use
    /// [`Cas::put_unsynced`] with one [`Cas::sync`] for a batch.
    pub fn put(&self, bytes: &[u8]) -> Result<ChunkHash> {
        self.put_with(bytes, true)
    }

    /// Store bytes without waiting for the disk. The chunk is not durable
    /// until [`Cas::sync`] returns; nothing may refer to it before then.
    pub fn put_unsynced(&self, bytes: &[u8]) -> Result<ChunkHash> {
        self.put_with(bytes, false)
    }

    fn put_with(&self, bytes: &[u8], durable: bool) -> Result<ChunkHash> {
        let h = ChunkHash::of(bytes);
        let path = self.chunk_path(h);
        if path.exists() {
            // A chunk written without a barrier may still be in flight; a
            // durable caller must not treat it as stored until the barrier.
            if durable {
                self.sync()?;
            }
            return Ok(h);
        }
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL).map_err(io_err(&path))?;
        let dir = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        // Unique per call: two writers of the same chunk (parallel snapshot
        // workers, or a repeated put) must not share a staging file.
        let seq = STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp =
            self.root
                .join("tmp")
                .join(format!("{}.{}.{seq}", h.to_hex(), std::process::id()));
        {
            let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
            f.write_all(&compressed).map_err(io_err(&tmp))?;
            if durable {
                f.sync_data().map_err(io_err(&tmp))?;
            }
        }
        fs::rename(&tmp, &path).map_err(io_err(&path))?;
        if durable {
            fs::File::open(dir)
                .and_then(|d| d.sync_all())
                .map_err(io_err(dir))?;
        }
        Ok(h)
    }

    /// Make every write so far durable: one barrier for the whole store
    /// filesystem (`syncfs`), covering data, renames and new directories.
    /// Chunks and manifests written with the `_unsynced` methods are durable
    /// once this returns.
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
        self.put_stream_with(reader, true)
    }

    /// As [`Cas::put_stream`], without waiting for the disk (see [`Cas::sync`]).
    pub fn put_stream_unsynced(&self, reader: impl Read) -> Result<(Vec<ChunkHash>, u64)> {
        self.put_stream_with(reader, false)
    }

    fn put_stream_with(&self, reader: impl Read, durable: bool) -> Result<(Vec<ChunkHash>, u64)> {
        let mut chunks = Vec::new();
        let mut size = 0u64;
        for chunk in fastcdc::v2020::StreamCDC::new(reader, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK) {
            let chunk = chunk.map_err(|e| CasError::Chunking(e.to_string()))?;
            size += chunk.length as u64;
            chunks.push(self.put_with(&chunk.data, durable)?);
        }
        Ok((chunks, size))
    }

    /// Store a manifest durably; returns its hash.
    pub fn put_manifest(&self, m: &Manifest) -> Result<ChunkHash> {
        self.put(&m.to_cbor()?)
    }

    /// Store a manifest without waiting for the disk (see [`Cas::sync`]).
    pub fn put_manifest_unsynced(&self, m: &Manifest) -> Result<ChunkHash> {
        self.put_unsynced(&m.to_cbor()?)
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
