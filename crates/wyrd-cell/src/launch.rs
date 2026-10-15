//! Stage one of a Cell launch: pick how the namespace is obtained, place the
//! process in a cgroup, and start stage two (`wyrd __cell-exec`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::cgroup::{CGROUP_ROOT, Cgroup};
use crate::exec::{LaunchMode, LaunchSpec};
use crate::{CellError, Result, io_err};

/// Parent cgroup used when running as root.
pub const ROOT_PARENT: &str = "wyrdline.slice";

/// Environment variable that opts in to running stage two through
/// `sudo -n` when unprivileged user namespaces are blocked.
pub const SUDO_HELPER_ENV: &str = "WYRD_PRIVILEGED_HELPER";

/// Whether stage two goes through `sudo -n`.
pub fn sudo_helper_enabled() -> bool {
    std::env::var(SUDO_HELPER_ENV).is_ok_and(|v| v == "sudo")
}

/// Decide how to obtain the mount namespace.
///
/// Preference: observe mode; already root (drop to `SUDO_UID`); an
/// unprivileged user namespace (`probe_userns` runs `wyrd __probe-userns`);
/// finally, if opted in, a root stage two through `sudo -n` that drops back
/// to the caller before running the agent.
pub fn choose_mode(
    observe: bool,
    probe_userns: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<LaunchMode> {
    if observe {
        return Ok(LaunchMode::Observe);
    }
    if nix::unistd::geteuid().is_root() {
        let id = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
        let (Some(uid), Some(gid)) = (id("SUDO_UID"), id("SUDO_GID")) else {
            return Err(CellError::Invalid(
                "running as root without SUDO_UID/SUDO_GID; refusing to run the agent as root"
                    .into(),
            ));
        };
        return Ok(LaunchMode::Root {
            uid,
            gid,
            groups: vec![gid],
        });
    }
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    match probe_userns() {
        Ok(()) => Ok(LaunchMode::UserNamespace { uid, gid }),
        Err(_) if sudo_helper_enabled() => {
            let groups = nix::unistd::getgroups().map_or_else(
                |_| vec![gid],
                |g| g.into_iter().map(nix::unistd::Gid::as_raw).collect(),
            );
            Ok(LaunchMode::Root { uid, gid, groups })
        }
        Err(reason) => Err(CellError::NoNamespace(reason)),
    }
}

/// Try the full unprivileged sequence in the current process: create a user
/// and mount namespace, map the caller, and change mount propagation. Under
/// Ubuntu's `AppArmor` restriction `unshare` succeeds but the namespace has no
/// capabilities, so only the later steps reveal the problem. Used by
/// `wyrd __probe-userns`; the process exits right after.
pub fn probe_userns_in_process() -> std::result::Result<(), String> {
    use nix::mount::{MsFlags, mount};
    use nix::sched::{CloneFlags, unshare};
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
        .map_err(|e| format!("unshare: {e}"))?;
    std::fs::write("/proc/self/setgroups", "deny").map_err(|e| format!("setgroups: {e}"))?;
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1"))
        .map_err(|e| format!("uid_map: {e}"))?;
    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1"))
        .map_err(|e| format!("gid_map: {e}"))?;
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .map_err(|e| format!("mount: {e}"))
}

fn systemd_user_available() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|d| Path::new(&d).join("systemd/private").exists())
}

/// A started Cell.
#[derive(Debug)]
pub struct Running {
    pub child: Child,
    pub cgroup: Option<Cgroup>,
    /// Overlay work directory to clean up as root after the run, if the
    /// overlay was mounted by root.
    pub root_workdir: Option<PathBuf>,
    /// Start gate stage two is blocked on (see [`Running::release`]).
    pub gate: Option<PathBuf>,
    /// Write end of the gate once stage two is waiting at it.
    pub writer: Option<std::fs::File>,
}

impl Running {
    /// Wait (up to 10 s) until stage two has finished its setup and is
    /// blocked at the start gate. Returns `false` if it never got there.
    pub fn wait_at_gate(&mut self) -> Result<bool> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let Some(gate) = self.gate.clone() else {
            return Ok(false);
        };
        let start = Instant::now();
        loop {
            // O_NONBLOCK: ENXIO until stage two has opened the FIFO for reading.
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
                .open(&gate)
            {
                Ok(f) => {
                    self.writer = Some(f);
                    return Ok(true);
                }
                Err(e) if e.raw_os_error() == Some(nix::libc::ENXIO) => {
                    if start.elapsed() > Duration::from_secs(10)
                        || matches!(self.child.try_wait(), Ok(Some(_)))
                    {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => return Err(io_err(&gate)(e)),
            }
        }
    }

    /// Let stage two exec the agent.
    pub fn release(&mut self) -> Result<()> {
        self.open_gate(b"g")
    }

    /// Make stage two abort instead of running the agent.
    pub fn abort(&mut self) -> Result<()> {
        self.open_gate(b"x")
    }

    fn open_gate(&mut self, byte: &[u8]) -> Result<()> {
        use std::io::Write as _;
        let Some(gate) = self.gate.take() else {
            return Ok(());
        };
        if self.writer.is_none() && !self.wait_at_gate_path(&gate)? {
            let _ = std::fs::remove_file(&gate);
            return Err(CellError::Sys(
                "stage two never reached the start gate".into(),
            ));
        }
        let r = match self.writer.take() {
            Some(mut f) => f.write_all(byte).map_err(io_err(&gate)),
            None => Ok(()),
        };
        let _ = std::fs::remove_file(&gate);
        r
    }

    fn wait_at_gate_path(&mut self, gate: &Path) -> Result<bool> {
        self.gate = Some(gate.to_path_buf());
        let ok = self.wait_at_gate()?;
        self.gate = None;
        Ok(ok)
    }
}

/// Start stage two for `spec` (already written to `spec_path`).
pub fn spawn(
    helper: &Path,
    spec: &mut LaunchSpec,
    spec_path: &Path,
    unit: &str,
) -> Result<Running> {
    let root = nix::unistd::geteuid().is_root();
    let mut cgroup = None;
    if root {
        let cg = Cgroup::create(&Path::new(CGROUP_ROOT).join(ROOT_PARENT), unit)?;
        spec.cgroup_procs = Some(cg.path().join("cgroup.procs"));
        cgroup = Some(cg);
    }
    let gate = spec_path.with_file_name("start.gate");
    let _ = std::fs::remove_file(&gate);
    nix::unistd::mkfifo(&gate, nix::sys::stat::Mode::from_bits_truncate(0o600))
        .map_err(|e| CellError::Sys(format!("mkfifo {}: {e}", gate.display())))?;
    spec.start_gate = Some(gate.clone());
    let bytes = serde_json::to_vec_pretty(spec).map_err(|e| CellError::Invalid(e.to_string()))?;
    std::fs::write(spec_path, bytes).map_err(io_err(spec_path))?;

    let use_systemd = !root && systemd_user_available();
    // A non-root caller in root mode elevates only stage two.
    let via_sudo = !root && matches!(spec.mode, LaunchMode::Root { .. });
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    if use_systemd {
        argv.extend(
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "-p",
                "Delegate=yes",
                "--unit",
                unit,
                "--",
            ]
            .map(Into::into),
        );
    }
    if via_sudo {
        argv.extend(["sudo", "-n", "--"].map(Into::into));
    }
    argv.push(helper.as_os_str().to_owned());
    argv.push("__cell-exec".into());
    argv.push(spec_path.as_os_str().to_owned());
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    let child = cmd.spawn().map_err(io_err(helper))?;

    if use_systemd {
        cgroup = Some(wait_for_scope(child.id(), unit)?);
    } else if !root {
        tracing::warn!("no systemd user manager: the Cell runs without its own cgroup");
    }
    let root_workdir = matches!(spec.mode, LaunchMode::Root { .. }).then(|| spec.work.clone());
    Ok(Running {
        child,
        cgroup,
        root_workdir,
        gate: Some(gate),
        writer: None,
    })
}

/// systemd moves the process into `<unit>.scope` before exec; wait for it.
fn wait_for_scope(pid: u32, unit: &str) -> Result<Cgroup> {
    let want = format!("{unit}.scope");
    let start = Instant::now();
    loop {
        if let Ok(cg) = Cgroup::of_pid(pid)
            && cg
                .path()
                .file_name()
                .is_some_and(|n| n.to_string_lossy() == want)
        {
            return Ok(cg);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(CellError::Sys(format!(
                "process {pid} never entered {want}"
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Environment for the agent: the caller's, plus session markers.
pub fn agent_env(session_id: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !k.starts_with("WYRD_INTERNAL_"))
        .collect();
    env.push(("WYRD_SESSION_ID".into(), session_id.into()));
    env.push(("WYRD_CELL".into(), "1".into()));
    env
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
