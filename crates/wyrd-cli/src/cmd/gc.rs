//! `wyrd gc --dry-run`: what collecting the content store would free.
//! Deleting is not implemented yet (#296).

use std::io::Write as _;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Report what a collection would keep and free; delete nothing.
    #[arg(long)]
    dry_run: bool,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    if !a.dry_run {
        writeln!(
            std::io::stderr(),
            "wyrd: only `wyrd gc --dry-run` exists yet: deleting chunks safely needs a store-wide \
             lock against a checkpoint writing at the same time (#296)"
        )?;
        return Ok(exit::USAGE);
    }
    let data = wyrd_cell::DataDir::from_env();
    let r = wyrd_cell::gc::dry_run(&data)?;
    let mut out = std::io::stdout().lock();
    if a.json {
        serde_json::to_writer_pretty(
            &mut out,
            &serde_json::json!({
                "dry_run": true,
                "kept": r.kept,
                "collectable": r.deleted,
                "bytes_freed": r.bytes_freed,
            }),
        )?;
        writeln!(out)?;
    } else {
        writeln!(out, "kept        {} chunks", r.kept)?;
        writeln!(
            out,
            "collectable {} chunks, {} bytes",
            r.deleted, r.bytes_freed
        )?;
        writeln!(out, "dry run: no chunks were deleted")?;
    }
    Ok(exit::OK)
}
