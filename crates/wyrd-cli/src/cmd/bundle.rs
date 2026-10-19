//! `wyrd export`: share a session as a signed, redacted `.wyrd` bundle.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::Context as _;
use wyrd_bundle::redact::is_secret_path;
use wyrd_bundle::{ExportInput, Redaction, SessionMeta};
use wyrd_cas::{Cas, ChunkHash, EntryKind};
use wyrd_cell::{CellError, DataDir, Session};
use wyrd_ledger::HostKey;
use wyrd_schema::{Event, EventBody, SessionState};

use crate::exit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RedactArg {
    /// The ingest-time redaction again (known secret formats, tokens).
    Standard,
    /// Standard plus home-directory user names and host names.
    Strict,
}

#[derive(Debug, clap::Args)]
pub struct ExportArgs {
    /// Session id or unique prefix.
    session: String,
    /// Bundle file to write.
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,
    /// Redaction applied to every string of every event.
    #[arg(long, value_enum, default_value = "strict")]
    redact: RedactArg,
    /// Leave out checkpoint content (`cas/`): share metadata only.
    #[arg(long)]
    no_content: bool,
    /// Print the summary as JSON.
    #[arg(long)]
    json: bool,
}

/// The host key, as the bundle signer.
struct HostSigner(HostKey);

impl wyrd_bundle::Signer for HostSigner {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key()
    }

    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.0.sign(msg)
    }
}

/// Open a session; `None` (after printing why) when there is no such session.
pub fn open_session(data: &DataDir, id: &str) -> anyhow::Result<Option<Session>> {
    match Session::open(data, id) {
        Ok(s) => Ok(Some(s)),
        Err(CellError::NotFound(what)) => {
            writeln!(std::io::stderr(), "wyrd: no {what}")?;
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// CAS chunks by BLAKE3 of their bytes.
type Chunks = BTreeMap<[u8; 32], Vec<u8>>;

/// Chunks of every checkpoint in `events`: each manifest and the chunks
/// of its files. With `withhold_secrets`, the content of secret-classified
/// paths is left out; returns the chunks and the number of such paths.
fn checkpoint_chunks(
    cas: &Cas,
    events: &[Event],
    withhold_secrets: bool,
) -> anyhow::Result<(Chunks, u64)> {
    let mut chunks = BTreeMap::new();
    let mut withheld: BTreeSet<Vec<u8>> = BTreeSet::new();
    for e in events {
        let EventBody::CheckpointCreated(c) = &e.body else {
            continue;
        };
        let hash = ChunkHash::from_hex(&c.manifest)
            .with_context(|| format!("checkpoint {} has a bad manifest hash", c.checkpoint_id))?;
        if chunks.contains_key(hash.as_bytes()) {
            continue;
        }
        let bytes = cas.get(hash)?;
        let manifest = wyrd_cas::Manifest::from_cbor(&bytes)?;
        chunks.insert(*hash.as_bytes(), bytes);
        for entry in manifest
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
        {
            if withhold_secrets && is_secret_path(&entry.path_str()) {
                withheld.insert(entry.path.to_vec());
                continue;
            }
            for chunk in &entry.chunks {
                if !chunks.contains_key(chunk.as_bytes()) {
                    chunks.insert(*chunk.as_bytes(), cas.get(*chunk)?);
                }
            }
        }
    }
    Ok((chunks, withheld.len() as u64))
}

fn kib(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let k = bytes as f64 / 1024.0;
    format!("{k:.1} KiB")
}

fn report(
    a: &ExportArgs,
    id: &str,
    redaction: Redaction,
    withheld: u64,
    sum: &wyrd_bundle::Summary,
) -> anyhow::Result<()> {
    let mut o = std::io::stdout().lock();
    if a.json {
        serde_json::to_writer_pretty(
            &mut o,
            &serde_json::json!({
                "session": id,
                "path": a.output,
                "redaction": redaction.as_str(),
                "content": !a.no_content,
                "withheld_files": withheld,
                "entries": sum.entries,
                "events": sum.events,
                "chunks": sum.chunks,
                "ledger_bytes": sum.ledger_bytes,
                "tar_bytes": sum.tar_bytes,
                "bundle_bytes": sum.bundle_bytes,
                "manifest_hash": sum.manifest_hash,
                "signer_fingerprint": sum.fingerprint,
            }),
        )?;
        writeln!(o)?;
        return Ok(());
    }
    writeln!(o, "exported  {} -> {}", id, a.output.display())?;
    writeln!(o, "redaction {}", redaction.as_str())?;
    writeln!(
        o,
        "events    {} (ledger {})",
        sum.events,
        kib(sum.ledger_bytes)
    )?;
    if a.no_content {
        writeln!(o, "content   none (--no-content)")?;
    } else {
        writeln!(
            o,
            "content   {} chunks ({withheld} secret-classified files withheld)",
            sum.chunks
        )?;
    }
    writeln!(
        o,
        "entries   {} (archive {}, bundle {})",
        sum.entries,
        kib(sum.tar_bytes),
        kib(sum.bundle_bytes)
    )?;
    writeln!(o, "manifest  {}", sum.manifest_hash)?;
    writeln!(o, "signer    {}", sum.fingerprint)?;
    Ok(())
}

pub fn export(a: &ExportArgs) -> anyhow::Result<u8> {
    let data = DataDir::from_env();
    let Some(s) = open_session(&data, &a.session)? else {
        return Ok(exit::NOT_FOUND);
    };
    let rec = s.record();
    if s.is_running() || matches!(rec.status, SessionState::Recording | SessionState::Degraded) {
        writeln!(
            std::io::stderr(),
            "wyrd: session {} is still recording; wait for the agent to exit or run `wyrd stop {}` first",
            rec.id,
            rec.id
        )?;
        return Ok(exit::CONFLICT);
    }
    let ledger_path = s.dir().ledger();
    let check = wyrd_ledger::verify(&ledger_path)?;
    if !check.ok() {
        writeln!(
            std::io::stderr(),
            "wyrd: the session ledger does not verify ({:?}); run `wyrd verify {}`",
            check.problem,
            rec.id
        )?;
        return Ok(exit::VERIFICATION_FAILED);
    }
    let events = wyrd_ledger::Ledger::open_read_only(&ledger_path)?.events_after(0, usize::MAX)?;
    let redaction = match a.redact {
        RedactArg::Standard => Redaction::Standard,
        RedactArg::Strict => Redaction::Strict,
    };
    let (chunks, withheld) = if a.no_content {
        (BTreeMap::new(), 0)
    } else {
        checkpoint_chunks(s.cas(), &events, redaction == Redaction::Strict)?
    };
    let meta = SessionMeta {
        id: rec.id.clone(),
        name: rec.name.clone(),
        agent: rec.agent.clone(),
        mode: rec.mode,
        status: rec.status,
        created_ms: rec.created_ms,
    };
    let signer = HostSigner(HostKey::load_or_create(&data.host_key())?);
    let out = wyrd_bundle::export(
        &ExportInput {
            session: &meta,
            events: &events,
            chunks: &chunks,
            content: !a.no_content,
            withheld_files: withheld,
            redaction,
            divergences: wyrd_divergence::view::project_json,
        },
        &signer,
    )?;
    wyrd_bundle::export::write_atomic(&a.output, &out.bytes)
        .with_context(|| format!("writing {}", a.output.display()))?;

    report(a, &rec.id, redaction, withheld, &out.summary)?;
    Ok(exit::OK)
}

/// zstd frame magic: the first bytes of every bundle.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Whether `path` is a bundle file (by content, not by name).
pub fn is_bundle(path: &std::path::Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .is_ok_and(|()| magic == ZSTD_MAGIC)
}

fn open_bundle(path: &std::path::Path) -> anyhow::Result<wyrd_bundle::Verification> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(wyrd_bundle::verify(
        std::io::BufReader::new(f),
        &wyrd_bundle::Options::default(),
    ))
}

fn mark(status: wyrd_bundle::read::CheckStatus) -> &'static str {
    match status {
        wyrd_bundle::read::CheckStatus::Passed => "✓",
        wyrd_bundle::read::CheckStatus::Failed => "✗",
        wyrd_bundle::read::CheckStatus::Skipped => "-",
    }
}

fn verification_json(path: &std::path::Path, v: &wyrd_bundle::Verification) -> serde_json::Value {
    let (bundle, error) = match &v.result {
        Ok(b) => (
            Some(serde_json::json!({
                "session": b.manifest.session,
                "redaction": b.manifest.redaction,
                "content": b.manifest.content,
                "events": b.manifest.ledger.events,
                "head_hash": b.manifest.ledger.head_hash,
                "manifest_hash": b.manifest_hash,
                "signer_fingerprint": b.fingerprint(),
            })),
            None,
        ),
        Err(e) => (
            None,
            Some(serde_json::json!({
                "stage": e.stage(),
                "seq": e.seq(),
                "message": e.to_string(),
            })),
        ),
    };
    serde_json::json!({
        "path": path,
        "ok": v.ok(),
        "checks": v.checks,
        "bundle": bundle,
        "error": error,
    })
}

/// Print a bundle verification report.
fn print_verification(
    path: &std::path::Path,
    v: &wyrd_bundle::Verification,
    json: bool,
) -> anyhow::Result<()> {
    let mut o = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut o, &verification_json(path, v))?;
        writeln!(o)?;
        return Ok(());
    }
    writeln!(o, "bundle    {}", path.display())?;
    if let Ok(b) = &v.result {
        let m = &b.manifest;
        let name = m.session.name.as_deref().unwrap_or("-");
        writeln!(
            o,
            "session   {} ({name}, {})",
            m.session.id, m.session.agent
        )?;
        writeln!(o, "events    {}", m.ledger.events)?;
        writeln!(o, "head      {}", m.ledger.head_hash)?;
        writeln!(o, "redaction {}", m.redaction.as_str())?;
        if m.content.included {
            writeln!(o, "content   {} chunks", m.content.chunks)?;
        } else {
            writeln!(o, "content   none (exported with --no-content)")?;
        }
        writeln!(o, "signer    {}", b.fingerprint())?;
    }
    for c in &v.checks {
        write!(o, "{} {}", mark(c.status), c.stage.describe())?;
        match &c.detail {
            Some(d) => writeln!(o, ": {d}")?,
            None => writeln!(o)?,
        }
    }
    match &v.result {
        Ok(_) => writeln!(o, "result    ✓ bundle verified")?,
        Err(e) => writeln!(o, "result    ✗ {e}")?,
    }
    Ok(())
}

/// `wyrd verify <file.wyrd>`.
pub fn verify(path: &std::path::Path, json: bool) -> anyhow::Result<u8> {
    let v = open_bundle(path)?;
    print_verification(path, &v, json)?;
    Ok(if v.ok() {
        exit::OK
    } else {
        exit::VERIFICATION_FAILED
    })
}

#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    /// Bundle file to import.
    file: PathBuf,
    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
}

/// `wyrd import <file.wyrd>`: verify, then add as a read-only session.
pub fn import(a: &ImportArgs) -> anyhow::Result<u8> {
    let verification = open_bundle(&a.file)?;
    if !verification.ok() {
        print_verification(&a.file, &verification, a.json)?;
        return Ok(exit::VERIFICATION_FAILED);
    }
    let Ok(b) = verification.result else {
        return Ok(exit::VERIFICATION_FAILED);
    };
    let m = &b.manifest;
    let spec = wyrd_cell::ImportSpec {
        name: m.session.name.clone(),
        agent: m.session.agent.clone(),
        mode: m.session.mode,
        created_ms: m.session.created_ms,
        events: b.events.clone(),
        chunks: b.chunks.values().cloned().collect(),
        source: wyrd_cell::ImportSource {
            original_id: m.session.id.clone(),
            manifest_hash: b.manifest_hash.clone(),
            signer_fingerprint: b.fingerprint(),
            redaction: m.redaction.as_str().into(),
            content: m.content.included,
            imported_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        },
    };
    let data = DataDir::from_env();
    let session = Session::import(&data, &spec)?;
    // The rebuilt ledger must carry the same chain as the bundle.
    let check = wyrd_ledger::verify(&session.dir().ledger())?;
    if !check.ok() || check.head_hash != m.ledger.head_hash || check.events != m.ledger.events {
        anyhow::bail!(
            "imported ledger of {} does not reproduce the bundle's chain ({} events, head {})",
            session.record().id,
            check.events,
            check.head_hash
        );
    }
    let mut out = std::io::stdout().lock();
    if a.json {
        serde_json::to_writer_pretty(
            &mut out,
            &serde_json::json!({
                "session": session.record().id,
                "original_id": m.session.id,
                "events": m.ledger.events,
                "checkpoints": session.record().checkpoints.len(),
                "chunks": b.chunks.len(),
                "head_hash": m.ledger.head_hash,
                "signer_fingerprint": b.fingerprint(),
            }),
        )?;
        writeln!(out)?;
    } else {
        writeln!(
            out,
            "imported  {} -> session {} (read-only)",
            a.file.display(),
            session.record().id
        )?;
        writeln!(out, "original  {} on the exporting host", m.session.id)?;
        writeln!(
            out,
            "events    {} (head {})",
            m.ledger.events, m.ledger.head_hash
        )?;
        writeln!(
            out,
            "content   {} checkpoints, {} chunks",
            session.record().checkpoints.len(),
            b.chunks.len()
        )?;
        writeln!(out, "signer    {}", b.fingerprint())?;
    }
    Ok(exit::OK)
}
