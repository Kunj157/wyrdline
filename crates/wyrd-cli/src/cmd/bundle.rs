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
