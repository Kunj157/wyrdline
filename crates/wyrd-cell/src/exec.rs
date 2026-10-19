//! Stage two of a Cell launch, run as `wyrd __cell-exec <spec>`: enter the
//! cgroup, create the mount namespace, mount the overlay over the workspace,
//! drop privileges, confine writes, and exec the agent.

use std::convert::Infallible;
use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};

use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::unistd::{Gid, Uid, setgid, setgroups, setuid};
use serde::{Deserialize, Serialize};

use crate::{CellError, Result, confine, io_err, sys_err};

/// How the mount namespace is obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchMode {
    /// Running as root: create the namespace, then drop to `uid`/`gid`.
    Root {
        uid: u32,
        gid: u32,
        groups: Vec<u32>,
    },
    /// Unprivileged: a user namespace mapping the caller to itself.
    UserNamespace { uid: u32, gid: u32 },
    /// No sandbox at all (observe mode).
    Observe,
}

/// Everything stage two needs, written to `launch.json` by stage one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub mode: LaunchMode,
    pub workspace: PathBuf,
    pub upper: PathBuf,
    pub work: PathBuf,
    /// `cgroup.procs` to join before anything else (root mode).
    pub cgroup_procs: Option<PathBuf>,
    /// Landlock rules (`write_allow`, `read_deny`, `tcp_connect_ports`).
    #[serde(flatten)]
    pub confine: crate::confine::Confinement,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    /// FIFO stage two reads one byte from before doing anything else, so
    /// stage one can finish setup (e.g. sensor registration) first.
    #[serde(default)]
    pub start_gate: Option<PathBuf>,
}

/// Block until stage one opens the gate. A gate closed without the go
/// byte means stage one gave up: abort instead of running unobserved.
fn wait_gate(gate: &Path) -> Result<()> {
    use std::io::Read as _;
    let mut f = fs::File::open(gate).map_err(io_err(gate))?;
    let mut b = [0u8; 1];
    match f.read(&mut b) {
        Ok(1) if b[0] == b'g' => Ok(()),
        _ => Err(CellError::Invalid("stage one aborted the launch".into())),
    }
}

/// Overlay option values cannot contain separators or escapes.
pub fn check_overlay_path(p: &Path) -> Result<()> {
    let s = p
        .to_str()
        .ok_or_else(|| CellError::Invalid(format!("{} is not UTF-8", p.display())))?;
    if s.chars()
        .any(|c| matches!(c, ',' | ':' | '\\' | '\n') || c.is_control())
    {
        return Err(CellError::Invalid(format!(
            "{s} contains a character overlayfs options cannot express (',', ':', '\\\\')"
        )));
    }
    Ok(())
}

pub fn overlay_options(spec: &LaunchSpec) -> Result<String> {
    for p in [&spec.workspace, &spec.upper, &spec.work] {
        check_overlay_path(p)?;
    }
    Ok(format!(
        "lowerdir={},upperdir={},workdir={},userxattr",
        spec.workspace.display(),
        spec.upper.display(),
        spec.work.display()
    ))
}

fn write_file(path: &str, value: &str) -> Result<()> {
    fs::write(path, value).map_err(io_err(Path::new(path)))
}

fn mount_overlay(spec: &LaunchSpec, owner: Option<(u32, u32)>) -> Result<()> {
    let opts = overlay_options(spec)?;
    // The overlay root takes its owner and mode from the upper directory, so
    // make them match the workspace or the agent could not create files there.
    let ws = fs::metadata(&spec.workspace).map_err(io_err(&spec.workspace))?;
    fs::set_permissions(&spec.upper, ws.permissions()).map_err(io_err(&spec.upper))?;
    if let Some((uid, gid)) = owner {
        nix::unistd::chown(
            &spec.upper,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .map_err(sys_err("chown upper"))?;
    }
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .map_err(sys_err("make mounts private"))?;
    mount(
        Some("wyrdline"),
        &spec.workspace,
        Some("overlay"),
        MsFlags::empty(),
        Some(opts.as_str()),
    )
    .map_err(sys_err("mount overlay"))
}

/// Run stage two. Only returns on error.
pub fn run(spec: &LaunchSpec) -> Result<Infallible> {
    if let Some(procs) = &spec.cgroup_procs {
        fs::write(procs, std::process::id().to_string()).map_err(io_err(procs))?;
    }
    match &spec.mode {
        LaunchMode::Root { uid, gid, groups } => {
            unshare(CloneFlags::CLONE_NEWNS).map_err(sys_err("unshare mount namespace"))?;
            mount_overlay(spec, Some((*uid, *gid)))?;
            let groups: Vec<Gid> = groups.iter().map(|g| Gid::from_raw(*g)).collect();
            setgroups(&groups).map_err(sys_err("setgroups"))?;
            setgid(Gid::from_raw(*gid)).map_err(sys_err("setgid"))?;
            setuid(Uid::from_raw(*uid)).map_err(sys_err("setuid"))?;
            if nix::unistd::geteuid().is_root() {
                return Err(CellError::Sys("failed to drop root privileges".into()));
            }
        }
        LaunchMode::UserNamespace { uid, gid } => {
            unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
                .map_err(|e| CellError::NoNamespace(e.to_string()))?;
            write_file("/proc/self/setgroups", "deny")?;
            write_file("/proc/self/uid_map", &format!("{uid} {uid} 1"))?;
            write_file("/proc/self/gid_map", &format!("{gid} {gid} 1"))?;
            mount_overlay(spec, None)?;
        }
        LaunchMode::Observe => {}
    }
    if spec.mode != LaunchMode::Observe {
        let enforcement = confine::restrict(&spec.confine)?;
        if enforcement != confine::Enforcement::Full {
            tracing::warn!(?enforcement, "Landlock confinement is not fully enforced");
        }
    }
    // Setup is done; block here so stage one registers the cgroup with the
    // sensor only now: the ledger then holds the agent's effects, not ours.
    if let Some(gate) = &spec.start_gate {
        wait_gate(gate)?;
    }
    let (prog, args) = spec
        .command
        .split_first()
        .ok_or_else(|| CellError::Invalid("empty command".into()))?;
    let err = std::process::Command::new(prog)
        .args(args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.cwd)
        .exec();
    Err(CellError::Invalid(format!("exec {prog}: {err}")))
}

/// Remove overlay's internal `work/work` directory, which the kernel creates
/// owned by root when the overlay was mounted by root. Only paths of the
/// form `…/branches/<id>/work` are accepted. Runs as root (`wyrd __cell-cleanup`).
pub fn cleanup_workdir(work: &Path) -> Result<()> {
    let ok = work.file_name().is_some_and(|n| n == "work")
        && work
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|n| n == "branches");
    if !ok || !work.is_absolute() {
        return Err(CellError::Invalid(format!(
            "refusing to clean {}",
            work.display()
        )));
    }
    let inner = work.join("work");
    match fs::remove_dir_all(&inner) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(&inner)(e)),
    }
}
