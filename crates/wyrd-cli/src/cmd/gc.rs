//! `wyrd gc`: delete the content store's chunks that no checkpoint reaches,
//! or with `--dry-run` only report what would be freed. Collection waits for a
//! checkpoint or import in progress, and gives up after a bounded wait (#296).

use std::io::Write as _;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Report what a collection would free; delete nothing.
    #[arg(long)]
    dry_run: bool,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let data = wyrd_cell::DataDir::from_env();
    let r = match wyrd_cell::gc::collect(&data, a.dry_run, wyrd_cell::gc::WAIT) {
        Ok(r) => r,
        Err(e @ wyrd_cell::CellError::Cas(wyrd_cas::CasError::Busy(_))) => {
            writeln!(std::io::stderr(), "wyrd: {e}")?;
            return Ok(exit::CONFLICT);
        }
        Err(e) => return Err(e.into()),
    };
    let mut out = std::io::stdout().lock();
    if a.json {
        let report = if a.dry_run {
            serde_json::json!({
                "dry_run": true,
                "kept": r.kept,
                "collectable": r.deleted,
                "bytes_freed": r.bytes_freed,
            })
        } else {
            serde_json::json!({
                "dry_run": false,
                "kept": r.kept,
                "deleted": r.deleted,
                "bytes_freed": r.bytes_freed,
            })
        };
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        writeln!(out, "kept        {} chunks", r.kept)?;
        if a.dry_run {
            writeln!(
                out,
                "collectable {} chunks, {} bytes",
                r.deleted, r.bytes_freed
            )?;
            writeln!(out, "dry run: no chunks were deleted")?;
        } else {
            writeln!(
                out,
                "deleted     {} chunks, {} bytes freed",
                r.deleted, r.bytes_freed
            )?;
        }
    }
    Ok(exit::OK)
}
