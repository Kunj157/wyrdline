//! Session commands: run, sessions, checkpoint(s), rewind, fork, diff, apply, stop, close.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{CellError, DataDir, Session};

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Record only: no sandbox, checkpoints, or rewind.
    #[arg(long)]
    observe: bool,
    /// Human-readable session name.
    #[arg(long)]
    name: Option<String>,
    /// Workspace to sandbox (default: current directory).
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Agent adapter name recorded in the ledger.
    #[arg(long, default_value = "generic")]
    agent: String,
    /// Agent command, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct SessionArg {
    /// Session id or unique prefix.
    session: String,
}

#[derive(Debug, clap::Args)]
pub struct ListCheckpointsArgs {
    session: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct RewindArgs {
    session: String,
    /// Checkpoint id (`c3`), branch name, or `head`.
    target: String,
    /// Start the agent again on the new branch.
    #[arg(long)]
    relaunch: bool,
}

#[derive(Debug, clap::Args)]
pub struct ForkArgs {
    session: String,
    target: String,
    /// Name of the new branch.
    #[arg(long)]
    name: String,
    #[arg(long)]
    relaunch: bool,
}

#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    session: String,
    a: String,
    b: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct ApplyArgs {
    session: String,
    target: String,
    /// Only list what would change.
    #[arg(long)]
    dry_run: bool,
    /// Apply even over files changed in the real workspace.
    #[arg(long)]
    force: bool,
    #[arg(long)]
    json: bool,
}

fn data() -> DataDir {
    DataDir::from_env()
}

fn out() -> std::io::StdoutLock<'static> {
    std::io::stdout().lock()
}

fn helper() -> anyhow::Result<PathBuf> {
    std::env::current_exe().context("locating the wyrd binary")
}

fn probe_userns(helper: &Path) -> impl FnOnce() -> Result<(), String> + '_ {
    move || match Command::new(helper).arg("__probe-userns").status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(read_userns_reason()),
        Err(e) => Err(e.to_string()),
    }
}

fn read_userns_reason() -> String {
    match std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") {
        Ok(v) if v.trim() == "1" => "AppArmor restricts unprivileged user namespaces".into(),
        _ => "the kernel refused to create a user namespace".into(),
    }
}

fn run_session(s: &mut Session) -> anyhow::Result<u8> {
    let helper = helper()?;
    let code = s.run(&helper, probe_userns(&helper))?;
    let rec = s.record();
    writeln!(
        out(),
        "wyrd: session {} finished (exit {code}); branch {}, {} checkpoint(s)",
        rec.id,
        rec.current_branch,
        rec.checkpoints.len()
    )?;
    Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

pub fn run(a: &RunArgs) -> anyhow::Result<u8> {
    let workspace = match &a.workspace {
        Some(w) => w.clone(),
        None => std::env::current_dir()?,
    };
    let doctor = wyrd_doctor::run(&wyrd_doctor::Env::new(
        "/",
        wyrd_doctor::Probes {
            landlock_abi: None,
            uid: rustix::process::getuid().as_raw(),
        },
    ));
    let mut s = Session::create(
        &data(),
        CreateOptions {
            workspace,
            command: a.command.clone(),
            observe: a.observe,
            name: a.name.clone(),
            agent_kind: a.agent.clone(),
            kernel_release: doctor.kernel_release.clone(),
            capabilities: doctor.capabilities(),
        },
    )?;
    writeln!(out(), "wyrd: session {} started", s.record().id)?;
    run_session(&mut s)
}

pub fn list(a: &ListArgs) -> anyhow::Result<u8> {
    let sessions = Session::list(&data())?;
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &sessions)?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    for r in sessions {
        writeln!(
            o,
            "{}  {:<10} {:>3} cp  branch {:<4} {}  {}",
            r.id,
            format!("{:?}", r.status).to_lowercase(),
            r.checkpoints.len(),
            r.current_branch,
            r.workspace.display(),
            r.command.join(" ")
        )?;
    }
    Ok(exit::OK)
}

pub fn checkpoint(a: &SessionArg) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let c = s.checkpoint(None)?;
    writeln!(
        out(),
        "{} on branch {}: {} ({} files, {} bytes read)",
        c.id,
        c.branch,
        c.manifest,
        c.files,
        c.bytes_read
    )?;
    Ok(exit::OK)
}

pub fn checkpoints(a: &ListCheckpointsArgs) -> anyhow::Result<u8> {
    let s = Session::open(&data(), &a.session)?;
    let r = s.record();
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(
            &mut o,
            &serde_json::json!({
                "current_branch": r.current_branch,
                "branches": r.branches,
                "checkpoints": r.checkpoints,
            }),
        )?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    for b in &r.branches {
        let mark = if b.id == r.current_branch { "*" } else { " " };
        let from = b
            .forked_from
            .as_deref()
            .map(|c| format!(" (from {c})"))
            .unwrap_or_default();
        writeln!(o, "{mark} branch {} [{}]{from}", b.name, b.id)?;
        for c in r.checkpoints.iter().filter(|c| c.branch == b.id) {
            writeln!(
                o,
                "    {:<5} seq {:<6} {} files  {}",
                c.id, c.seq, c.files, c.manifest
            )?;
        }
    }
    Ok(exit::OK)
}

fn rewind_or_fork(
    session: &str,
    target: &str,
    name: Option<&str>,
    relaunch: bool,
) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), session)?;
    let b = s.rewind(target, name)?;
    writeln!(
        out(),
        "wyrd: now on branch {} (from {})",
        b.name,
        b.forked_from.as_deref().unwrap_or("-")
    )?;
    if relaunch {
        run_session(&mut s)
    } else {
        Ok(exit::OK)
    }
}

pub fn rewind(a: &RewindArgs) -> anyhow::Result<u8> {
    rewind_or_fork(&a.session, &a.target, None, a.relaunch)
}

pub fn fork(a: &ForkArgs) -> anyhow::Result<u8> {
    rewind_or_fork(&a.session, &a.target, Some(&a.name), a.relaunch)
}

pub fn diff(a: &DiffArgs) -> anyhow::Result<u8> {
    let s = Session::open(&data(), &a.session)?;
    let changes = s.diff(&a.a, &a.b)?;
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &changes)?;
        writeln!(o)?;
    } else {
        for c in &changes {
            let sign = match c.kind {
                wyrd_cell::diff::ChangeKind::Added => "+",
                wyrd_cell::diff::ChangeKind::Removed => "-",
                wyrd_cell::diff::ChangeKind::Modified => "~",
            };
            writeln!(o, "{sign} {:<8} {}", c.entry, c.path)?;
        }
        writeln!(o, "{} change(s)", changes.len())?;
    }
    Ok(exit::OK)
}

pub fn apply(a: &ApplyArgs) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let report = match s.apply(&a.target, a.dry_run, a.force) {
        Ok(r) => r,
        Err(CellError::Conflicts(paths)) => {
            let mut e = std::io::stderr().lock();
            writeln!(
                e,
                "wyrd: apply refused; changed in the real workspace since the session started:"
            )?;
            for p in paths {
                writeln!(e, "  {p}")?;
            }
            writeln!(e, "Re-run with --force to overwrite them.")?;
            return Ok(exit::CONFLICT);
        }
        Err(e) => return Err(e.into()),
    };
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &report)?;
        writeln!(o)?;
    } else {
        for op in &report.ops {
            writeln!(o, "{}", serde_json::to_string(op)?)?;
        }
        if !report.conflicts.is_empty() {
            writeln!(o, "conflicts: {}", report.conflicts.join(", "))?;
        }
        writeln!(
            o,
            "{}",
            if report.applied {
                "applied"
            } else {
                "dry run: nothing written"
            }
        )?;
    }
    Ok(exit::OK)
}

pub fn stop(a: &SessionArg) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let stopped = s.stop()?;
    writeln!(out(), "{}", if stopped { "stopped" } else { "not running" })?;
    Ok(exit::OK)
}

pub fn close(a: &SessionArg) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let seal = s.close("closed by user")?;
    writeln!(
        out(),
        "sealed at seq {} (b3:{})",
        seal.head_seq,
        hex::encode(seal.head_hash)
    )?;
    Ok(exit::OK)
}

pub fn cell_exec(spec_path: &Path) -> anyhow::Result<u8> {
    let bytes =
        std::fs::read(spec_path).with_context(|| format!("reading {}", spec_path.display()))?;
    let spec: wyrd_cell::exec::LaunchSpec = serde_json::from_slice(&bytes)?;
    match wyrd_cell::exec::run(&spec) {
        Ok(never) => match never {},
        Err(e) => {
            eprintln!("wyrd: cell: {e}");
            Ok(127)
        }
    }
}
