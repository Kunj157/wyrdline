//! `wyrd gc`: delete the content store's chunks that no checkpoint reaches,
//! or with `--dry-run` only report what would be freed. Collection waits for a
//! checkpoint or import in progress, and gives up after a bounded wait (#296).
//! With `--keep-days` or `--keep-sessions`, closed sessions beyond the limits
//! are removed first, with their ledgers (#318); running, busy and imported
//! sessions are never removed.

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
    /// Remove closed sessions created more than this many days ago. Opt-in.
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u32).range(1..))]
    keep_days: Option<u32>,
    /// Keep the newest N closed sessions and remove the rest. Opt-in. A
    /// session is kept if either limit keeps it.
    #[arg(long, value_name = "N")]
    keep_sessions: Option<usize>,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let data = wyrd_cell::DataDir::from_env();
    let retention = wyrd_cell::gc::Retention {
        keep_days: a.keep_days,
        keep_sessions: a.keep_sessions,
    };
    let c = match wyrd_cell::gc::collect_retained(&data, a.dry_run, wyrd_cell::gc::WAIT, &retention)
    {
        Ok(c) => c,
        Err(e @ wyrd_cell::CellError::Cas(wyrd_cas::CasError::Busy(_))) => {
            writeln!(std::io::stderr(), "wyrd: {e}")?;
            return Ok(exit::CONFLICT);
        }
        Err(e) => return Err(e.into()),
    };
    let r = &c.report;
    let mut out = std::io::stdout().lock();
    if a.json {
        let report = if a.dry_run {
            serde_json::json!({
                "dry_run": true,
                "kept": r.kept,
                "collectable": r.deleted,
                "bytes_freed": r.bytes_freed,
                "expired": c.expired,
                "kept_busy": c.kept_busy,
            })
        } else {
            serde_json::json!({
                "dry_run": false,
                "kept": r.kept,
                "deleted": r.deleted,
                "bytes_freed": r.bytes_freed,
                "expired": c.expired,
                "kept_busy": c.kept_busy,
            })
        };
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        if retention.is_set() {
            let verb = if a.dry_run { "would expire" } else { "expired" };
            writeln!(out, "sessions  {verb} {}", c.expired.len())?;
            for id in &c.kept_busy {
                writeln!(out, "sessions  kept {id} (running or busy)")?;
            }
        }
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
