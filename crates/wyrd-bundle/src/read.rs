//! Reading a bundle: verify everything before exposing anything.
//!
//! The input is untrusted. [`read`] streams the zstd frame and the ustar
//! archive inside it and, in this order:
//!
//! 1. checks every entry header (regular files only, safe v1 paths, sizes
//!    within [`Limits`]) and the total and ratio of decompressed bytes;
//! 2. verifies `signature.ed25519` over `manifest.json` (the first two
//!    entries) before parsing the manifest;
//! 3. parses the manifest and refuses unknown versions and fields;
//! 4. checks every further entry against the manifest: expected path and
//!    order, size, BLAKE3 (and a CAS chunk's name against its content);
//! 5. recomputes the ledger hash chain and compares its head and length
//!    with the manifest.
//!
//! Only then is a [`Bundle`] returned. Nothing here panics on any input
//! and every allocation is bounded by [`Limits`] (fuzzed in `tests/fuzz`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use serde::Serialize;
use wyrd_schema::chain::{GENESIS, Hash, chain_hash};
use wyrd_schema::{Event, from_canonical_cbor, to_canonical_cbor};

use crate::archive::{BLOCK, checksum};
use crate::format::{
    self, ANNOTATIONS, CAS_DIR, CLAIMS, DIVERGENCES, FileEntry, LEDGER, MANIFEST, Manifest,
    PathProblem, SIGNATURE, SIGNATURE_LEN,
};

/// Bounds on what a reader accepts, so a hostile bundle cannot exhaust
/// memory or time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Archive entries, `manifest.json` and `signature.ed25519` included.
    pub max_entries: u64,
    /// Bytes of one entry.
    pub max_entry_bytes: u64,
    /// Bytes of `manifest.json`.
    pub max_manifest_bytes: u64,
    /// Decompressed bytes of the whole archive.
    pub max_total_bytes: u64,
    /// Largest ratio of decompressed to compressed bytes...
    pub max_ratio: u64,
    /// ...checked once this many bytes are decompressed (small inputs of
    /// zeros compress extremely well and are harmless).
    pub ratio_after_bytes: u64,
    /// zstd window: the decoder's buffer, declared by the frame header.
    pub max_window_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 250_000,
            max_entry_bytes: 512 << 20,
            max_manifest_bytes: 64 << 20,
            max_total_bytes: 4 << 30,
            max_ratio: 1_000,
            ratio_after_bytes: 64 << 20,
            max_window_bytes: 32 << 20,
        }
    }
}

/// The verification steps, in the order a report lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// zstd frame, ustar headers, entry paths and types, size limits.
    Archive,
    /// `signature.ed25519` over `manifest.json`.
    Signature,
    /// `manifest.json` parses, has a known version, and is consistent.
    Manifest,
    /// Every entry matches the manifest (path, order, size, BLAKE3).
    Files,
    /// The ledger hash chain recomputes to the manifest head.
    Chain,
}

impl Stage {
    pub const ALL: [Stage; 5] = [
        Stage::Archive,
        Stage::Signature,
        Stage::Manifest,
        Stage::Files,
        Stage::Chain,
    ];

    pub fn describe(self) -> &'static str {
        match self {
            Stage::Archive => "archive structure and limits",
            Stage::Signature => "signature over the manifest",
            Stage::Manifest => "manifest",
            Stage::Files => "file hashes",
            Stage::Chain => "ledger hash chain",
        }
    }
}

/// Why a bundle was rejected. Each variant names what failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    // ---- archive ----
    #[error("not a zstd-compressed bundle: {0}")]
    Decompress(String),
    #[error("the archive ends early")]
    Truncated,
    #[error("entry {index}: bad tar header ({reason})")]
    BadHeader { index: u64, reason: &'static str },
    #[error("entry {path:?} is a symbolic link")]
    Symlink { path: String },
    #[error("entry {path:?} is a hard link")]
    Hardlink { path: String },
    #[error("entry {path:?} is a device or fifo")]
    Device { path: String },
    #[error("entry {path:?} has unsupported type {typeflag:?}")]
    UnsupportedEntry { path: String, typeflag: char },
    #[error("entry {path:?} is an absolute path")]
    AbsolutePath { path: String },
    #[error("entry {path:?} escapes the bundle (`..`)")]
    PathTraversal { path: String },
    #[error("entry {path:?} is not a valid bundle path")]
    InvalidPath { path: String },
    #[error("entry {path:?} appears twice")]
    DuplicateEntry { path: String },
    #[error("more than {limit} entries")]
    TooManyEntries { limit: u64 },
    #[error("entry {path:?} is {size} bytes, limit is {limit}")]
    EntryTooLarge { path: String, size: u64, limit: u64 },
    #[error("more than {limit} decompressed bytes (size bomb)")]
    TotalTooLarge { limit: u64 },
    #[error("compression ratio above {limit}:1 (size bomb)")]
    CompressionRatio { limit: u64 },
    #[error("data after the end of the archive")]
    TrailingData,
    // ---- signature ----
    #[error("the first entry is {found:?}, expected {expected:?}")]
    MissingEntry {
        expected: &'static str,
        found: String,
    },
    #[error("signature.ed25519 is {0} bytes, expected {SIGNATURE_LEN}")]
    SignatureSize(u64),
    #[error("the signature over manifest.json does not verify")]
    BadSignature,
    // ---- manifest ----
    #[error("manifest.json is invalid: {0}")]
    BadManifest(String),
    #[error("unknown bundle format {format:?} version {version}")]
    UnknownVersion { format: String, version: u32 },
    #[error("the manifest names signer {manifest}, the signature is by {signature}")]
    SignerMismatch { manifest: String, signature: String },
    #[error("signed by {actual}, expected {expected}")]
    UnexpectedSigner { expected: String, actual: String },
    // ---- files ----
    #[error("entry {path:?} is not in the manifest")]
    UnexpectedEntry { path: String },
    #[error("entry {path:?} is out of order")]
    OutOfOrder { path: String },
    #[error("{path:?} is listed in the manifest but missing")]
    MissingFile { path: String },
    #[error("{path:?} is {actual} bytes, the manifest says {expected}")]
    SizeMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("{path:?} does not match its hash in the manifest")]
    FileHash { path: String },
    #[error("chunk {chunk} does not match the manifest")]
    ChunkModified { chunk: String },
    #[error("chunk {chunk} does not hash to its name")]
    ChunkName { chunk: String },
    #[error("ledger.cbor-seq does not match the manifest: event modified at seq {seq}")]
    EventModified { seq: u64 },
    // ---- chain ----
    #[error("ledger record {seq}: {reason}")]
    BadRecord { seq: u64, reason: String },
    #[error("sequence gap: expected seq {expected}, found {found}")]
    SequenceGap { expected: u64, found: u64 },
    #[error("chain hash mismatch at seq {seq}")]
    HashMismatch { seq: u64 },
    #[error(
        "the chain has {actual} events and head {actual_head}, the manifest says {expected} and {expected_head}"
    )]
    HeadMismatch {
        expected: u64,
        actual: u64,
        expected_head: String,
        actual_head: String,
    },
}

impl ReadError {
    pub fn stage(&self) -> Stage {
        use ReadError as E;
        match self {
            E::Decompress(_)
            | E::Truncated
            | E::BadHeader { .. }
            | E::Symlink { .. }
            | E::Hardlink { .. }
            | E::Device { .. }
            | E::UnsupportedEntry { .. }
            | E::AbsolutePath { .. }
            | E::PathTraversal { .. }
            | E::InvalidPath { .. }
            | E::DuplicateEntry { .. }
            | E::TooManyEntries { .. }
            | E::EntryTooLarge { .. }
            | E::TotalTooLarge { .. }
            | E::CompressionRatio { .. }
            | E::TrailingData => Stage::Archive,
            E::MissingEntry { .. } | E::SignatureSize(_) | E::BadSignature => Stage::Signature,
            E::BadManifest(_)
            | E::UnknownVersion { .. }
            | E::SignerMismatch { .. }
            | E::UnexpectedSigner { .. } => Stage::Manifest,
            E::UnexpectedEntry { .. }
            | E::OutOfOrder { .. }
            | E::MissingFile { .. }
            | E::SizeMismatch { .. }
            | E::FileHash { .. }
            | E::ChunkModified { .. }
            | E::ChunkName { .. }
            | E::EventModified { .. } => Stage::Files,
            E::BadRecord { .. }
            | E::SequenceGap { .. }
            | E::HashMismatch { .. }
            | E::HeadMismatch { .. } => Stage::Chain,
        }
    }

    /// The ledger seq the failure is about, when there is one.
    pub fn seq(&self) -> Option<u64> {
        match self {
            ReadError::EventModified { seq }
            | ReadError::BadRecord { seq, .. }
            | ReadError::HashMismatch { seq } => Some(*seq),
            ReadError::SequenceGap { found, .. } => Some(*found),
            _ => None,
        }
    }
}

/// A verified bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub manifest: Manifest,
    /// `b3:` + hex of `manifest.json`.
    pub manifest_hash: String,
    pub public_key: [u8; 32],
    /// The events in seq order (dense from 1), chain verified.
    pub events: Vec<Event>,
    /// CAS chunks by BLAKE3 of their bytes.
    pub chunks: BTreeMap<Hash, Vec<u8>>,
    pub claims_json: Vec<u8>,
    pub divergences_json: Vec<u8>,
    pub annotations: Option<Vec<u8>>,
}

impl Bundle {
    pub fn fingerprint(&self) -> String {
        format::fingerprint(&self.public_key)
    }
}

/// Options of [`read`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub limits: Limits,
    /// Require this signer (its public key); any other is
    /// [`ReadError::UnexpectedSigner`].
    pub signer: Option<[u8; 32]>,
}

/// One line of a verification report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub stage: Stage,
    pub status: CheckStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    /// Not reached because an earlier step failed.
    Skipped,
}

/// The outcome of reading a bundle with the check-by-check report.
#[derive(Debug)]
pub struct Verification {
    pub checks: Vec<Check>,
    pub result: Result<Bundle, ReadError>,
}

impl Verification {
    pub fn ok(&self) -> bool {
        self.result.is_ok()
    }
}

/// Read and verify a bundle, with a report of every check.
pub fn verify(input: impl Read, options: &Options) -> Verification {
    let mut done = Vec::new();
    let result = Reader::new(options, &mut done).run(input);
    let failed = result.as_ref().err();
    let checks = Stage::ALL
        .iter()
        .map(|&stage| {
            let (status, detail) = if done.contains(&stage) {
                (CheckStatus::Passed, None)
            } else if failed.is_some_and(|e| e.stage() == stage) {
                (CheckStatus::Failed, failed.map(ToString::to_string))
            } else {
                (CheckStatus::Skipped, None)
            };
            Check {
                stage,
                status,
                detail,
            }
        })
        .collect();
    Verification { checks, result }
}

/// Read and verify a bundle.
pub fn read(input: impl Read, options: &Options) -> Result<Bundle, ReadError> {
    let mut done = Vec::new();
    Reader::new(options, &mut done).run(input)
}

/// Counts the compressed bytes the decoder consumes.
struct Counting<R> {
    inner: R,
    count: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

/// The decompressed stream with the total and ratio limits applied.
struct Stream<R: Read> {
    dec: ruzstd::decoding::StreamingDecoder<Counting<R>, ruzstd::decoding::FrameDecoder>,
    out: u64,
    limits: Limits,
}

impl<R: Read> Stream<R> {
    /// Fill `buf` completely; `Ok(false)` on a clean end before any byte.
    fn fill(&mut self, buf: &mut [u8]) -> Result<bool, ReadError> {
        let mut got = 0;
        while got < buf.len() {
            let n = self
                .dec
                .read(&mut buf[got..])
                .map_err(|e| ReadError::Decompress(e.to_string()))?;
            if n == 0 {
                return if got == 0 {
                    Ok(false)
                } else {
                    Err(ReadError::Truncated)
                };
            }
            got += n;
            self.account(n as u64)?;
        }
        Ok(true)
    }

    fn account(&mut self, n: u64) -> Result<(), ReadError> {
        self.out += n;
        let l = &self.limits;
        if self.out > l.max_total_bytes {
            return Err(ReadError::TotalTooLarge {
                limit: l.max_total_bytes,
            });
        }
        let compressed = self.dec.get_ref().count.max(1);
        if self.out > l.ratio_after_bytes && self.out / compressed > l.max_ratio {
            return Err(ReadError::CompressionRatio { limit: l.max_ratio });
        }
        Ok(())
    }

    /// Read exactly `size` bytes of an entry and its block padding. The
    /// buffer grows with the data actually decompressed, never ahead of it.
    fn entry(&mut self, size: u64) -> Result<Vec<u8>, ReadError> {
        const STEP: usize = 64 << 10;
        let len = usize::try_from(size).map_err(|_| ReadError::Truncated)?;
        let mut data = Vec::with_capacity(len.min(STEP));
        while data.len() < len {
            let have = data.len();
            data.resize(have + (len - have).min(STEP), 0);
            if !self.fill(&mut data[have..])? {
                return Err(ReadError::Truncated);
            }
        }
        let pad = crate::archive::padding(size);
        if pad > 0 {
            let mut p = [0u8; BLOCK];
            if !self.fill(&mut p[..pad])? {
                return Err(ReadError::Truncated);
            }
        }
        Ok(data)
    }

    /// After the end-of-archive blocks: only zero bytes, then the end of
    /// the frame and of the input.
    fn finish(&mut self) -> Result<(), ReadError> {
        let mut buf = [0u8; BLOCK];
        loop {
            let n = self
                .dec
                .read(&mut buf)
                .map_err(|e| ReadError::Decompress(e.to_string()))?;
            if n == 0 {
                break;
            }
            self.account(n as u64)?;
            if buf[..n].iter().any(|&b| b != 0) {
                return Err(ReadError::TrailingData);
            }
        }
        let mut one = [0u8; 1];
        match self.dec.get_mut().read(&mut one) {
            Ok(0) => Ok(()),
            Ok(_) => Err(ReadError::TrailingData),
            Err(e) => Err(ReadError::Decompress(e.to_string())),
        }
    }
}

/// A parsed entry header.
struct Header {
    path: String,
    size: u64,
}

/// A numeric header field: optional leading spaces, octal digits, then
/// only NUL or space.
fn octal(field: &[u8]) -> Option<u64> {
    let start = field.iter().position(|&b| b != b' ')?;
    let field = &field[start..];
    let len = field
        .iter()
        .position(|b| !(b'0'..=b'7').contains(b))
        .unwrap_or(field.len());
    if len == 0 || field[len..].iter().any(|&b| b != 0 && b != b' ') {
        return None;
    }
    field[..len].iter().try_fold(0u64, |acc, &d| {
        acc.checked_mul(8)?.checked_add(u64::from(d - b'0'))
    })
}

fn parse_header(h: &[u8; BLOCK], index: u64) -> Result<Header, ReadError> {
    let bad = |reason| ReadError::BadHeader { index, reason };
    if &h[257..263] != b"ustar\0" {
        return Err(bad("not a ustar header"));
    }
    if octal(&h[148..156]) != Some(u64::from(checksum(h))) {
        return Err(bad("checksum mismatch"));
    }
    let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
    if h[345..500].iter().any(|&b| b != 0) {
        return Err(bad("name prefix is not supported"));
    }
    let path = String::from_utf8_lossy(&h[..name_end]).into_owned();
    if std::str::from_utf8(&h[..name_end]).is_err() || h[name_end..100].iter().any(|&b| b != 0) {
        return Err(ReadError::InvalidPath { path });
    }
    match h[156] {
        b'0' | 0 => {}
        b'1' => return Err(ReadError::Hardlink { path }),
        b'2' => return Err(ReadError::Symlink { path }),
        b'3' | b'4' | b'6' => return Err(ReadError::Device { path }),
        t => {
            return Err(ReadError::UnsupportedEntry {
                path,
                typeflag: char::from(t),
            });
        }
    }
    if let Err(p) = format::check_path(&path) {
        return Err(path_error(p, path));
    }
    let size = octal(&h[124..136]).ok_or_else(|| bad("bad size field"))?;
    Ok(Header { path, size })
}

struct Reader<'a> {
    options: &'a Options,
    done: &'a mut Vec<Stage>,
    entries: u64,
    seen: BTreeSet<String>,
}

impl<'a> Reader<'a> {
    fn new(options: &'a Options, done: &'a mut Vec<Stage>) -> Self {
        Reader {
            options,
            done,
            entries: 0,
            seen: BTreeSet::new(),
        }
    }

    /// The next entry's header; `None` at the end-of-archive marker.
    fn next_header<R: Read>(&mut self, s: &mut Stream<R>) -> Result<Option<Header>, ReadError> {
        let mut h = [0u8; BLOCK];
        if !s.fill(&mut h)? {
            return Err(ReadError::Truncated);
        }
        if h.iter().all(|&b| b == 0) {
            if !s.fill(&mut h)? || h.iter().any(|&b| b != 0) {
                return Err(ReadError::Truncated);
            }
            return Ok(None);
        }
        let header = parse_header(&h, self.entries)?;
        self.entries += 1;
        if self.entries > self.options.limits.max_entries {
            return Err(ReadError::TooManyEntries {
                limit: self.options.limits.max_entries,
            });
        }
        if !self.seen.insert(header.path.clone()) {
            return Err(ReadError::DuplicateEntry { path: header.path });
        }
        Ok(Some(header))
    }

    fn run(mut self, input: impl Read) -> Result<Bundle, ReadError> {
        let limits = self.options.limits;
        let dec = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
            Counting {
                inner: input,
                count: 0,
            },
            limits.max_window_bytes,
        )
        .map_err(|e| ReadError::Decompress(e.to_string()))?;
        let mut s = Stream {
            dec,
            out: 0,
            limits,
        };

        let manifest_json = self.fixed_entry(&mut s, MANIFEST, limits.max_manifest_bytes)?;
        let signature = self.fixed_entry(&mut s, SIGNATURE, SIGNATURE_LEN as u64)?;
        let public_key = verify_signature(&manifest_json, &signature)?;
        self.done.push(Stage::Signature);

        let manifest = parse_manifest(&manifest_json, &public_key, self.options)?;
        self.done.push(Stage::Manifest);

        let mut files = BTreeMap::new();
        for expected in &manifest.files {
            let Some(h) = self.next_header(&mut s)? else {
                return Err(ReadError::MissingFile {
                    path: expected.path.clone(),
                });
            };
            if h.path == MANIFEST || h.path == SIGNATURE {
                return Err(ReadError::DuplicateEntry { path: h.path });
            }
            if h.path != expected.path {
                return Err(
                    if manifest
                        .files
                        .binary_search_by(|f| f.path.cmp(&h.path))
                        .is_ok()
                    {
                        ReadError::OutOfOrder { path: h.path }
                    } else {
                        ReadError::UnexpectedEntry { path: h.path }
                    },
                );
            }
            if h.size != expected.size {
                return Err(ReadError::SizeMismatch {
                    path: h.path,
                    expected: expected.size,
                    actual: h.size,
                });
            }
            let data = s.entry(h.size)?;
            check_file(expected, &data)?;
            files.insert(h.path, data);
        }
        if let Some(h) = self.next_header(&mut s)? {
            return Err(ReadError::UnexpectedEntry { path: h.path });
        }
        s.finish()?;
        self.done.push(Stage::Files);
        self.done.push(Stage::Archive);

        let ledger = files.remove(LEDGER).unwrap_or_default();
        let events = check_chain(&ledger, &manifest)?;
        self.done.push(Stage::Chain);

        let mut chunks = BTreeMap::new();
        let mut rest = BTreeMap::new();
        for (path, data) in files {
            match path.strip_prefix(CAS_DIR).and_then(chunk_hash) {
                Some(hash) => {
                    chunks.insert(hash, data);
                }
                None => {
                    rest.insert(path, data);
                }
            }
        }
        Ok(Bundle {
            manifest_hash: format::b3(&manifest_json),
            manifest,
            public_key,
            events,
            chunks,
            claims_json: rest.remove(CLAIMS).unwrap_or_default(),
            divergences_json: rest.remove(DIVERGENCES).unwrap_or_default(),
            annotations: rest.remove(ANNOTATIONS),
        })
    }

    /// One of the two leading entries, which must come first and in order.
    fn fixed_entry<R: Read>(
        &mut self,
        s: &mut Stream<R>,
        name: &'static str,
        limit: u64,
    ) -> Result<Vec<u8>, ReadError> {
        let h = self.next_header(s)?.ok_or(ReadError::MissingEntry {
            expected: name,
            found: "the end of the archive".into(),
        })?;
        if h.path != name {
            return Err(ReadError::MissingEntry {
                expected: name,
                found: h.path,
            });
        }
        if name == SIGNATURE && h.size != limit {
            return Err(ReadError::SignatureSize(h.size));
        }
        if h.size > limit {
            return Err(ReadError::EntryTooLarge {
                path: h.path,
                size: h.size,
                limit,
            });
        }
        s.entry(h.size)
    }
}

fn verify_signature(manifest_json: &[u8], signature: &[u8]) -> Result<[u8; 32], ReadError> {
    let (pk, sig) = signature
        .split_at_checked(32)
        .ok_or(ReadError::BadSignature)?;
    let pk: [u8; 32] = pk.try_into().map_err(|_| ReadError::BadSignature)?;
    let sig = ed25519_dalek::Signature::from_slice(sig).map_err(|_| ReadError::BadSignature)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&pk).map_err(|_| ReadError::BadSignature)?;
    key.verify_strict(&format::signed_message(manifest_json), &sig)
        .map_err(|_| ReadError::BadSignature)?;
    Ok(pk)
}

#[derive(serde::Deserialize)]
struct VersionOnly {
    format: String,
    format_version: u32,
}

fn path_error(problem: PathProblem, path: String) -> ReadError {
    match problem {
        PathProblem::Absolute => ReadError::AbsolutePath { path },
        PathProblem::Traversal => ReadError::PathTraversal { path },
        PathProblem::Invalid => ReadError::InvalidPath { path },
    }
}

/// Parse and check `manifest.json` (already known to be signed by `pk`).
pub fn parse_manifest(
    bytes: &[u8],
    pk: &[u8; 32],
    options: &Options,
) -> Result<Manifest, ReadError> {
    let bad = |m: String| ReadError::BadManifest(m);
    // The version first, so a newer bundle is reported as such and not as
    // an unknown field.
    let v: VersionOnly = serde_json::from_slice(bytes).map_err(|e| bad(e.to_string()))?;
    if v.format != format::FORMAT || v.format_version != format::FORMAT_VERSION {
        return Err(ReadError::UnknownVersion {
            format: v.format,
            version: v.format_version,
        });
    }
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| bad(e.to_string()))?;
    let signer = hex::encode(pk);
    if m.signer.public_key != signer || m.signer.fingerprint != format::fingerprint(pk) {
        return Err(ReadError::SignerMismatch {
            manifest: m.signer.fingerprint.clone(),
            signature: format::fingerprint(pk),
        });
    }
    if let Some(expected) = options.signer
        && expected != *pk
    {
        return Err(ReadError::UnexpectedSigner {
            expected: format::fingerprint(&expected),
            actual: format::fingerprint(pk),
        });
    }
    check_file_list(&m, &options.limits)?;
    Ok(m)
}

/// The manifest's file list: safe sorted paths of the layout, within the
/// limits, with the required files, consistent with `content`.
fn check_file_list(m: &Manifest, limits: &Limits) -> Result<(), ReadError> {
    let bad = |m: String| ReadError::BadManifest(m);
    if m.files.len() as u64 + 2 > limits.max_entries {
        return Err(ReadError::TooManyEntries {
            limit: limits.max_entries,
        });
    }
    let mut total: u64 = 0;
    let mut chunks = 0u64;
    for (i, f) in m.files.iter().enumerate() {
        format::check_path(&f.path).map_err(|p| path_error(p, f.path.clone()))?;
        if f.path == MANIFEST || f.path == SIGNATURE {
            return Err(bad(format!("{MANIFEST} lists itself or the signature")));
        }
        if i > 0 && m.files[i - 1].path >= f.path {
            return Err(if m.files[i - 1].path == f.path {
                ReadError::DuplicateEntry {
                    path: f.path.clone(),
                }
            } else {
                bad("files are not sorted by path".into())
            });
        }
        if f.size > limits.max_entry_bytes {
            return Err(ReadError::EntryTooLarge {
                path: f.path.clone(),
                size: f.size,
                limit: limits.max_entry_bytes,
            });
        }
        total = total.saturating_add(f.size);
        if total > limits.max_total_bytes {
            return Err(ReadError::TotalTooLarge {
                limit: limits.max_total_bytes,
            });
        }
        if parse_b3(&f.blake3).is_none() {
            return Err(bad(format!("{}: bad hash {:?}", f.path, f.blake3)));
        }
        if f.path.starts_with(CAS_DIR) {
            chunks += 1;
        }
    }
    for required in [LEDGER, CLAIMS, DIVERGENCES] {
        if m.files
            .binary_search_by(|f| f.path.as_str().cmp(required))
            .is_err()
        {
            return Err(bad(format!("{required} is not listed")));
        }
    }
    if chunks != m.content.chunks || (!m.content.included && chunks > 0) {
        return Err(bad(format!(
            "content lists {} chunks (included: {}), the files {chunks}",
            m.content.chunks, m.content.included
        )));
    }
    if parse_b3(&m.ledger.head_hash).is_none() {
        return Err(bad(format!("bad head hash {:?}", m.ledger.head_hash)));
    }
    Ok(())
}

fn parse_b3(s: &str) -> Option<Hash> {
    let hex_part = s.strip_prefix("b3:")?;
    if !format::is_chunk_name(hex_part) {
        return None;
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_part, &mut out).ok()?;
    Some(out)
}

fn chunk_hash(name: &str) -> Option<Hash> {
    parse_b3(&format!("b3:{name}"))
}

fn check_file(expected: &FileEntry, data: &[u8]) -> Result<(), ReadError> {
    let actual = *blake3::hash(data).as_bytes();
    let chunk = expected.path.strip_prefix(CAS_DIR);
    if Some(actual) != parse_b3(&expected.blake3) {
        return Err(if let Some(chunk) = chunk {
            ReadError::ChunkModified {
                chunk: chunk.to_string(),
            }
        } else if expected.path == LEDGER {
            // Name the event that was changed, when the records still parse.
            match check_records(data, None).err().and_then(|e| e.seq()) {
                Some(seq) => ReadError::EventModified { seq },
                None => ReadError::FileHash {
                    path: expected.path.clone(),
                },
            }
        } else {
            ReadError::FileHash {
                path: expected.path.clone(),
            }
        });
    }
    if let Some(chunk) = chunk
        && chunk_hash(chunk) != Some(actual)
    {
        return Err(ReadError::ChunkName {
            chunk: chunk.to_string(),
        });
    }
    Ok(())
}

/// A parsed record: event CBOR, chain hash, and the input after it.
pub type Record<'a> = (&'a [u8], &'a [u8], &'a [u8]);

/// One `ledger.cbor-seq` record: `[bstr event_cbor, bstr(32) hash]`.
pub fn parse_record(input: &[u8]) -> Result<Record<'_>, String> {
    let (&first, rest) = input.split_first().ok_or("empty record")?;
    if first != 0x82 {
        return Err("not a two-element array".into());
    }
    let (event, rest) = bstr(rest)?;
    let (hash, rest) = bstr(rest)?;
    if hash.len() != 32 {
        return Err("hash is not 32 bytes".into());
    }
    Ok((event, hash, rest))
}

/// A definite-length CBOR byte string.
fn bstr(input: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let (&b, rest) = input.split_first().ok_or("truncated")?;
    if b >> 5 != 2 {
        return Err("not a byte string".into());
    }
    let (len, rest): (u64, &[u8]) = match b & 0x1f {
        n @ 0..=23 => (u64::from(n), rest),
        n @ 24..=27 => {
            let width = 1usize << (n - 24);
            let (bytes, rest) = rest.split_at_checked(width).ok_or("truncated length")?;
            (
                bytes.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x)),
                rest,
            )
        }
        _ => return Err("indefinite or reserved length".into()),
    };
    let len = usize::try_from(len).map_err(|_| "length too large")?;
    rest.split_at_checked(len)
        .ok_or_else(|| "truncated byte string".into())
}

/// Walk the records, decode each event, and recompute the chain. With
/// `head`, also compare the final count and head.
fn check_records(ledger: &[u8], head: Option<(u64, Hash)>) -> Result<Vec<Event>, ReadError> {
    let mut rest = ledger;
    let mut prev = GENESIS;
    let mut events = Vec::new();
    let mut seq = 0u64;
    while !rest.is_empty() {
        seq += 1;
        let bad = |reason: String| ReadError::BadRecord { seq, reason };
        let (cbor, hash, tail) = parse_record(rest).map_err(bad)?;
        rest = tail;
        let event = from_canonical_cbor(cbor).map_err(|e| bad(e.to_string()))?;
        if event.seq != seq {
            return Err(ReadError::SequenceGap {
                expected: seq,
                found: event.seq,
            });
        }
        let canonical = to_canonical_cbor(&event).map_err(|e| bad(e.to_string()))?;
        if canonical != cbor {
            return Err(bad("event is not canonically encoded".into()));
        }
        let h = chain_hash(&prev, cbor);
        if h.as_slice() != hash {
            return Err(ReadError::HashMismatch { seq });
        }
        prev = h;
        events.push(event);
    }
    if let Some((count, expected)) = head
        && (count != seq || expected != prev)
    {
        return Err(ReadError::HeadMismatch {
            expected: count,
            actual: seq,
            expected_head: format!("b3:{}", hex::encode(expected)),
            actual_head: format!("b3:{}", hex::encode(prev)),
        });
    }
    Ok(events)
}

fn check_chain(ledger: &[u8], m: &Manifest) -> Result<Vec<Event>, ReadError> {
    let head = parse_b3(&m.ledger.head_hash).unwrap_or(GENESIS);
    check_records(ledger, Some((m.ledger.events, head)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_fields() {
        assert_eq!(octal(b"00000002322\0"), Some(1234));
        assert_eq!(octal(b"0000644\0"), Some(0o644));
        assert_eq!(octal(b"  1234 \0"), Some(0o1234));
        assert_eq!(octal(b"\0\0\0"), None);
        assert_eq!(octal(b"0009\0"), None);
        assert_eq!(octal(b"12\x0034"), None);
        assert_eq!(octal(&[0x80, 0, 0, 0]), None);
    }

    #[test]
    fn records_parse_definite_byte_strings_only() {
        let mut r = vec![0x82, 0x43, 1, 2, 3, 0x58, 32];
        r.extend([7u8; 32]);
        let (e, h, rest) = parse_record(&r).unwrap();
        assert_eq!((e, h.len(), rest.len()), (&[1u8, 2, 3][..], 32, 0));
        assert!(parse_record(&[0x82, 0x5f]).is_err());
        assert!(
            parse_record(&[0x82, 0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_err()
        );
        assert!(parse_record(&[0x83]).is_err());
        assert!(parse_record(&[]).is_err());
    }
}
