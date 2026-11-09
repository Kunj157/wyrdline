//! `wyrd restore <dir>`: restore a backup (`wyrd backup`) into the data
//! directory (#318). The data directory must be absent or empty; the copy is
//! verified against the backup's manifest before it is put in place.

use std::io::Write as _;
use std::path::PathBuf;

use wyrd_cell::CellError;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// A backup directory made by `wyrd backup`.
    dir: PathBuf,
    /// Print the manifest of the restored backup as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let data = wyrd_cell::DataDir::from_env();
    let m = match wyrd_cell::backup::restore(&a.dir, &data) {
        Ok(m) => m,
        Err(e @ (CellError::Busy(_) | CellError::Occupied(_))) => {
            writeln!(std::io::stderr(), "wyrd: {e}")?;
            return Ok(exit::CONFLICT);
        }
        Err(e) => return Err(e.into()),
    };
    if !m.host_key_included {
        writeln!(
            std::io::stderr(),
            "wyrd: warning: the backup has no host private key; the next session closed here is sealed with a new key"
        )?;
    }
    let mut out = std::io::stdout().lock();
    if a.json {
        serde_json::to_writer_pretty(&mut out, &m)?;
        writeln!(out)?;
    } else {
        let sealed = m.sessions.iter().filter(|s| s.sealed).count();
        writeln!(out, "restored  {}", data.root().display())?;
        writeln!(out, "sessions  {} ({sealed} sealed)", m.sessions.len())?;
        writeln!(
            out,
            "chunks    {} reachable from {} checkpoints",
            m.chunks, m.roots
        )?;
        writeln!(out, "result    ✓ verified against the backup manifest")?;
    }
    Ok(exit::OK)
}
