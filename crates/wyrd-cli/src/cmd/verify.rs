//! `wyrd verify`.

use std::io::Write as _;
use std::path::PathBuf;

use wyrd_ledger::Problem;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Session id (or unique prefix), or the path of a ledger or bundle file.
    target: String,
    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(args: &Args) -> anyhow::Result<u8> {
    let path = PathBuf::from(&args.target);
    if path.is_file() && crate::cmd::bundle::is_bundle(&path) {
        return crate::cmd::bundle::verify(&path, args.json);
    }
    let path = if path.exists() {
        path
    } else {
        wyrd_cell::Session::open(&wyrd_cell::DataDir::from_env(), &args.target)?
            .dir()
            .ledger()
    };
    let v = wyrd_ledger::verify(&path)?;
    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &v)?;
        writeln!(out)?;
    } else {
        writeln!(out, "session   {}", v.session_id)?;
        writeln!(out, "events    {}", v.events)?;
        writeln!(out, "head      {}", v.head_hash)?;
        match &v.signer_fingerprint {
            Some(fp) => writeln!(out, "sealed    yes, signer {fp}")?,
            None => writeln!(
                out,
                "sealed    no (session still open or not closed cleanly)"
            )?,
        }
        match &v.problem {
            None => writeln!(out, "result    ✓ chain intact")?,
            Some(p) => writeln!(out, "result    ✗ {}", describe(p))?,
        }
    }
    Ok(if v.ok() {
        exit::OK
    } else {
        exit::VERIFICATION_FAILED
    })
}

fn describe(p: &Problem) -> String {
    match p {
        Problem::SequenceGap { expected, found } => {
            format!("sequence gap: expected seq {expected}, found {found}")
        }
        Problem::BrokenLink { seq } => format!("chain link broken at seq {seq}"),
        Problem::HashMismatch { seq } => format!("event modified at seq {seq}"),
        Problem::BadEvent { seq, reason } => format!("invalid event at seq {seq}: {reason}"),
        Problem::SealHeadMismatch {
            sealed_seq,
            actual_seq,
        } => format!("seal covers {sealed_seq} events but the ledger has {actual_seq}"),
        Problem::BadSignature => "seal signature does not verify".to_string(),
    }
}
