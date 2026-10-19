//! `wyrd doctor`.

use std::io::Write as _;
use std::process::Command;

use anyhow::Context as _;
use wyrd_doctor::{Env, Probes};

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(args: &Args) -> anyhow::Result<u8> {
    let probes = Probes {
        landlock_abi: landlock_abi_from_child(),
        uid: rustix::process::getuid().as_raw(),
    };
    let mut env = Env::new("/", probes);
    env.mask = std::env::var("WYRD_DOCTOR_MASK").ok();
    let report = wyrd_doctor::run(&env);

    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        write!(out, "{}", wyrd_doctor::render_text(&report))?;
    }
    Ok(if report.cell_mode {
        exit::OK
    } else {
        exit::CAPABILITY_MISSING
    })
}

/// Restricting a process with Landlock is irreversible, so the ABI is probed
/// in a short-lived child running `wyrd __probe-landlock`.
pub fn landlock_abi_from_child() -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    let out = Command::new(exe).arg("__probe-landlock").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

pub fn probe_landlock() -> anyhow::Result<u8> {
    use landlock::{ABI, Access, AccessFs, LandlockStatus, Ruleset, RulesetAttr};
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V1))
        .context("handle access")?
        .create()
        .context("create ruleset")?
        .restrict_self()
        .context("restrict self")?;
    let abi = match status.landlock {
        LandlockStatus::Available {
            effective_abi,
            kernel_abi,
        } => kernel_abi.unwrap_or(effective_abi as i32),
        LandlockStatus::NotEnabled | LandlockStatus::NotImplemented => return Ok(exit::USAGE),
    };
    writeln!(std::io::stdout(), "{abi}")?;
    Ok(exit::OK)
}
