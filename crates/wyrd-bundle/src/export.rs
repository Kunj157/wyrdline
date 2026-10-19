//! Writing a bundle: redact every event, re-chain, sign the manifest, pack.
//!
//! [`export`] is a pure function of its input (no clock, no randomness, no
//! file system), so the same session exported twice gives the same bytes.

use std::collections::BTreeMap;

use wyrd_schema::chain::{GENESIS, Hash, chain_hash};
use wyrd_schema::{ClaimExtracted, Event, EventBody, SCHEMA_ID, to_canonical_cbor};

use crate::archive;
use crate::format::{
    self, CAS_DIR, CLAIMS, Content, DIVERGENCES, FileEntry, LEDGER, LedgerSummary, MANIFEST,
    Manifest, Redaction, SIGNATURE, SessionMeta, SignerInfo,
};
use crate::redact::{
    KnownHosts, Level, host_pseudonym, hosts_in, map_strings, redact_json, redact_str,
    visit_strings,
};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("events must be dense from seq 1: expected {expected}, found {found}")]
    Sequence { expected: u64, found: u64 },
    #[error("event {seq}: {reason}")]
    Event { seq: u64, reason: String },
    #[error("chunk b3:{0} does not hash to its name")]
    ChunkMismatch(String),
    #[error("{0} does not fit a ustar header")]
    Entry(String),
    #[error("encoding {what}: {reason}")]
    Encode { what: &'static str, reason: String },
}

/// Signs the manifest. Implemented for an ed25519 key; the CLI wraps the
/// host key the ledger seals with.
pub trait Signer {
    fn public_key(&self) -> [u8; 32];
    fn sign(&self, msg: &[u8]) -> [u8; 64];
}

impl Signer for ed25519_dalek::SigningKey {
    fn public_key(&self) -> [u8; 32] {
        self.verifying_key().to_bytes()
    }

    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        ed25519_dalek::Signer::sign(self, msg).to_bytes()
    }
}

/// Everything a bundle is made from.
#[derive(Debug, Clone, Copy)]
pub struct ExportInput<'a> {
    pub session: &'a SessionMeta,
    /// The session's events in ledger order (seq 1, 2, …).
    pub events: &'a [Event],
    /// CAS chunks to include, by BLAKE3 of their bytes. Empty and
    /// `content = false` for `--no-content`.
    pub chunks: &'a BTreeMap<Hash, Vec<u8>>,
    pub content: bool,
    pub withheld_files: u64,
    pub redaction: Redaction,
}

/// What [`export`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// `b3:` + hex of `manifest.json`.
    pub manifest_hash: String,
    pub fingerprint: String,
    /// Archive entries, `manifest.json` and `signature.ed25519` included.
    pub entries: usize,
    pub events: u64,
    pub chunks: u64,
    pub ledger_bytes: u64,
    /// Uncompressed archive size.
    pub tar_bytes: u64,
    /// Size of the bundle file.
    pub bundle_bytes: u64,
}

/// The bundle bytes and their summary.
#[derive(Debug, Clone)]
pub struct Exported {
    pub bytes: Vec<u8>,
    pub summary: Summary,
}

/// Every host name a session mentions: DNS queries, connect targets, and
/// hosts in URLs or after `user@` in any string.
pub fn session_hosts(events: &[Event]) -> KnownHosts {
    let mut hosts = Vec::new();
    for e in events {
        match &e.body {
            EventBody::DnsQuery(q) => hosts.push(q.name.clone()),
            EventBody::NetConnect(c) => hosts.extend(c.dns_name.clone()),
            _ => {}
        }
        if let Ok(v) = serde_json::to_value(&e.body) {
            visit_strings(&v, &mut |s| hosts.extend(hosts_in(s)));
        }
    }
    KnownHosts::new(hosts)
}

/// Redact every string of an event's payload at `level`. Under strict
/// redaction, fields that hold a host name are pseudonymised whole and
/// every occurrence of a `known` host is replaced.
pub fn redact_event(event: &Event, level: Level, known: &KnownHosts) -> Result<Event, ExportError> {
    let bad = |reason: String| ExportError::Event {
        seq: event.seq,
        reason,
    };
    let mut body = serde_json::to_value(&event.body).map_err(|e| bad(e.to_string()))?;
    if let Some(payload) = body.get_mut("payload") {
        *payload = redact_json(payload, level);
        if level >= Level::Strict {
            map_strings(payload, &|s| known.replace(s));
        }
    }
    let mut body: EventBody = serde_json::from_value(body).map_err(|e| bad(e.to_string()))?;
    if level >= Level::Strict {
        match &mut body {
            EventBody::DnsQuery(q) => q.name = host_pseudonym(&q.name),
            EventBody::NetConnect(c) => {
                if let Some(n) = &mut c.dns_name {
                    *n = host_pseudonym(n);
                }
            }
            _ => {}
        }
    }
    Ok(Event {
        body,
        ..event.clone()
    })
}

/// One `ledger.cbor-seq` record: `[event_cbor, chain_hash]`.
fn ledger_record(cbor: Vec<u8>, hash: &Hash, out: &mut Vec<u8>) -> Result<(), ExportError> {
    let record = ciborium::Value::Array(vec![
        ciborium::Value::Bytes(cbor),
        ciborium::Value::Bytes(hash.to_vec()),
    ]);
    ciborium::into_writer(&record, out).map_err(|e| ExportError::Encode {
        what: "ledger record",
        reason: e.to_string(),
    })
}

fn json(what: &'static str, v: &impl serde::Serialize) -> Result<Vec<u8>, ExportError> {
    let mut out = serde_json::to_vec_pretty(v).map_err(|e| ExportError::Encode {
        what,
        reason: e.to_string(),
    })?;
    out.push(b'\n');
    Ok(out)
}

/// Check the sequence and redact every event.
fn redact_all(events: &[Event], level: Level) -> Result<(Vec<Event>, KnownHosts), ExportError> {
    let known = if level >= Level::Strict {
        session_hosts(events)
    } else {
        KnownHosts::default()
    };
    let mut out = Vec::with_capacity(events.len());
    for (i, ev) in events.iter().enumerate() {
        let expected = i as u64 + 1;
        if ev.seq != expected {
            return Err(ExportError::Sequence {
                expected,
                found: ev.seq,
            });
        }
        out.push(redact_event(ev, level, &known)?);
    }
    Ok((out, known))
}

/// `ledger.cbor-seq` and its chain head.
fn encode_ledger(events: &[Event]) -> Result<(Vec<u8>, Hash), ExportError> {
    let mut ledger = Vec::new();
    let mut head = GENESIS;
    for ev in events {
        let cbor = to_canonical_cbor(ev).map_err(|e| ExportError::Event {
            seq: ev.seq,
            reason: e.to_string(),
        })?;
        head = chain_hash(&head, &cbor);
        ledger_record(cbor, &head, &mut ledger)?;
    }
    Ok((ledger, head))
}

/// Every entry but the manifest and its signature, by path.
fn content_files(
    events: &[Event],
    ledger: Vec<u8>,
    chunks: &BTreeMap<Hash, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>, ExportError> {
    let claims: Vec<&ClaimExtracted> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ClaimExtracted(c) => Some(c),
            _ => None,
        })
        .collect();
    let divergences = wyrd_divergence::view::project(events);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    files.insert(CLAIMS.into(), json("claims", &claims)?);
    files.insert(DIVERGENCES.into(), json("divergences", &divergences)?);
    files.insert(LEDGER.into(), ledger);
    for (hash, bytes) in chunks {
        if blake3::hash(bytes).as_bytes() != hash {
            return Err(ExportError::ChunkMismatch(hex::encode(hash)));
        }
        files.insert(format!("{CAS_DIR}{}", hex::encode(hash)), bytes.clone());
    }
    Ok(files)
}

/// Build a signed bundle.
pub fn export(input: &ExportInput<'_>, signer: &dyn Signer) -> Result<Exported, ExportError> {
    let level = input.redaction.level();
    let (events, known) = redact_all(input.events, level)?;
    let (ledger, head) = encode_ledger(&events)?;
    let ledger_bytes = ledger.len() as u64;
    let files = content_files(&events, ledger, input.chunks)?;

    let public_key = signer.public_key();
    let fingerprint = format::fingerprint(&public_key);
    let manifest = Manifest {
        format: format::FORMAT.into(),
        format_version: format::FORMAT_VERSION,
        event_schema: SCHEMA_ID.into(),
        session: SessionMeta {
            name: input
                .session
                .name
                .as_deref()
                .map(|n| known.replace(&redact_str(n, level))),
            ..input.session.clone()
        },
        redaction: input.redaction,
        content: Content {
            included: input.content,
            chunks: input.chunks.len() as u64,
            withheld_files: input.withheld_files,
        },
        ledger: LedgerSummary {
            events: events.len() as u64,
            head_hash: format!("b3:{}", hex::encode(head)),
        },
        signer: SignerInfo {
            public_key: hex::encode(public_key),
            fingerprint: fingerprint.clone(),
        },
        files: files
            .iter()
            .map(|(path, bytes)| FileEntry {
                path: path.clone(),
                size: bytes.len() as u64,
                blake3: format::b3(bytes),
            })
            .collect(),
    };
    let manifest_json = json("manifest", &manifest)?;
    let mut signature = public_key.to_vec();
    signature.extend_from_slice(&signer.sign(&format::signed_message(&manifest_json)));

    let entries: Vec<(&str, &[u8])> = [
        (MANIFEST, manifest_json.as_slice()),
        (SIGNATURE, signature.as_slice()),
    ]
    .into_iter()
    .chain(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
    .collect();
    let entry_count = entries.len();
    let tar = archive::pack(entries.iter().copied())
        .ok_or_else(|| ExportError::Entry("an entry of 8 GiB or more".into()))?;
    let bytes = archive::compress(&tar);
    Ok(Exported {
        summary: Summary {
            manifest_hash: format::b3(&manifest_json),
            fingerprint,
            entries: entry_count,
            events: events.len() as u64,
            chunks: input.chunks.len() as u64,
            ledger_bytes,
            tar_bytes: tar.len() as u64,
            bundle_bytes: bytes.len() as u64,
        },
        bytes,
    })
}

/// Write `bytes` to `path` atomically: a temporary file in the same
/// directory, synced, then renamed over `path`. On any failure the
/// temporary file is removed and `path` is untouched.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::path::Path;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other(format!("{} has no file name", path.display())))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    } else if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    result
}
