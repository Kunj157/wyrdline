//! `wyrd backup <dir>`: a consistent, verified copy of the data directory
//! (#318). Refuses while a session runs, and exits 4 (conflict) on a
//! destination that is occupied. The host private key is left out unless
//! `--include-key` is given.

use std::io::Write as _;
use std::path::PathBuf;

use wyrd_cell::CellError;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Directory to create (or an existing empty one). Must be outside the data directory.
    dir: PathBuf,
    /// Also copy the host private key (`keys/host.key`). Whoever holds it can
    /// sign as this host, so keep the backup as private as the key.
    #[arg(long)]
    include_key: bool,
    /// Print the manifest as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let data = wyrd_cell::DataDir::from_env();
    let m = match wyrd_cell::backup::backup(&data, &a.dir, a.include_key) {
        Ok(m) => m,
        Err(e @ (CellError::Busy(_) | CellError::Occupied(_))) => {
            writeln!(std::io::stderr(), "wyrd: {e}")?;
            return Ok(exit::CONFLICT);
        }
        Err(e) => return Err(e.into()),
    };
    if a.include_key {
        writeln!(
            std::io::stderr(),
            "wyrd: warning: the backup contains the host private key; keep it private"
        )?;
    }
    let mut out = std::io::stdout().lock();
    if a.json {
        serde_json::to_writer_pretty(&mut out, &m)?;
        writeln!(out)?;
    } else {
        let sealed = m.sessions.iter().filter(|s| s.sealed).count();
        writeln!(out, "backup    {}", a.dir.display())?;
        writeln!(out, "sessions  {} ({sealed} sealed)", m.sessions.len())?;
        writeln!(
            out,
            "chunks    {} reachable from {} checkpoints",
            m.chunks, m.roots
        )?;
        match &m.host_fingerprint {
            Some(fp) if m.host_key_included => {
                writeln!(out, "host key  {fp}, private key included")?;
            }
            Some(fp) => writeln!(
                out,
                "host key  {fp}, public key only (private key left out)"
            )?,
            None => writeln!(out, "host key  none in this data directory")?,
        }
        writeln!(out, "result    ✓ verified")?;
    }
    Ok(exit::OK)
}
