//! Kernel capability detection (`wyrd doctor`).
//!
//! Every check reads files below a configurable root so tests can run
//! against fake `/sys` and `/proc` trees. Values that need a syscall (the
//! Landlock ABI, the current UID) are passed in through [`Probes`].

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Degraded,
    Missing,
    Unknown,
}

/// Stable identifiers of the checks, used by `WYRD_DOCTOR_MASK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    Kernel,
    Btf,
    Ringbuf,
    CgroupV2,
    CgroupDelegation,
    Landlock,
    BpfLsm,
    Userns,
    OverlayUserns,
    CgroupSockAddr,
    Sensord,
}

impl CheckId {
    pub const ALL: [CheckId; 11] = [
        CheckId::Kernel,
        CheckId::Btf,
        CheckId::Ringbuf,
        CheckId::CgroupV2,
        CheckId::CgroupDelegation,
        CheckId::Landlock,
        CheckId::BpfLsm,
        CheckId::Userns,
        CheckId::OverlayUserns,
        CheckId::CgroupSockAddr,
        CheckId::Sensord,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CheckId::Kernel => "kernel",
            CheckId::Btf => "btf",
            CheckId::Ringbuf => "ringbuf",
            CheckId::CgroupV2 => "cgroup_v2",
            CheckId::CgroupDelegation => "cgroup_delegation",
            CheckId::Landlock => "landlock",
            CheckId::BpfLsm => "bpf_lsm",
            CheckId::Userns => "userns",
            CheckId::OverlayUserns => "overlay_userns",
            CheckId::CgroupSockAddr => "cgroup_sock_addr",
            CheckId::Sensord => "sensord",
        }
    }

    fn parse(s: &str) -> Option<CheckId> {
        CheckId::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Whether Cell mode cannot run without this capability.
    pub fn required_for_cell(self) -> bool {
        matches!(
            self,
            CheckId::CgroupV2 | CheckId::Landlock | CheckId::OverlayUserns
        )
    }

    /// Whether kernel capture cannot run without this capability.
    pub fn required_for_capture(self) -> bool {
        matches!(self, CheckId::Btf | CheckId::Ringbuf | CheckId::CgroupV2)
    }
}

/// One line of the report.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: CheckId,
    pub title: &'static str,
    pub status: Status,
    pub detail: String,
    /// What the user loses when the status is not `ok`.
    pub consequence: &'static str,
    pub required_for_cell: bool,
    pub required_for_capture: bool,
}

/// Full report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub kernel_release: String,
    pub checks: Vec<Check>,
    pub cell_mode: bool,
    pub kernel_capture: bool,
    pub kernel_network_enforcement: bool,
    pub masked: Vec<String>,
}

impl Report {
    /// Capability name → usable, as stored in `meta.session.started`.
    pub fn capabilities(&self) -> BTreeMap<String, bool> {
        self.checks
            .iter()
            .map(|c| (c.id.as_str().to_string(), c.status == Status::Ok))
            .collect()
    }

    pub fn get(&self, id: CheckId) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }
}

/// Results of probes that need syscalls; filled by the caller.
#[derive(Debug, Clone, Default)]
pub struct Probes {
    /// Landlock ABI reported by `landlock_create_ruleset`, `None` if unavailable.
    pub landlock_abi: Option<i32>,
    pub uid: u32,
}

/// Inputs to [`run`].
#[derive(Debug, Clone)]
pub struct Env {
    /// Filesystem root (normally `/`).
    pub root: PathBuf,
    pub probes: Probes,
    /// Value of `WYRD_DOCTOR_MASK`.
    pub mask: Option<String>,
    /// Path of the `wyrd-sensord` socket, relative to `root`.
    pub sensord_socket: PathBuf,
}

impl Env {
    pub fn new(root: impl Into<PathBuf>, probes: Probes) -> Self {
        Env {
            root: root.into(),
            probes,
            mask: None,
            sensord_socket: PathBuf::from("run/wyrdline/sensord.sock"),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel.trim_start_matches('/'))
    }

    fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.path(rel))
            .ok()
            .map(|s| s.trim().to_string())
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }
}

/// Kernel version triple parsed from a release string such as `6.17.0-14-generic`.
pub fn parse_kernel_version(release: &str) -> Option<(u32, u32, u32)> {
    let core = release
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()?;
    let mut it = core.split('.').map(|p| p.parse::<u32>().ok());
    let major = it.next()??;
    let minor = it.next().flatten().unwrap_or(0);
    let patch = it.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

fn at_least(v: Option<(u32, u32, u32)>, major: u32, minor: u32) -> Option<bool> {
    v.map(|(a, b, _)| (a, b) >= (major, minor))
}

/// Run every check.
pub fn run(env: &Env) -> Report {
    let release = env.read("/proc/sys/kernel/osrelease").unwrap_or_default();
    let version = parse_kernel_version(&release);
    let lsms: Vec<String> = env
        .read("/sys/kernel/security/lsm")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let cgroup_v2 = env.exists("/sys/fs/cgroup/cgroup.controllers");

    let mut checks = vec![
        check_kernel(&release, version),
        check_btf(env),
        check_ringbuf(version),
        check_cgroup_v2(cgroup_v2),
        check_delegation(env, cgroup_v2),
        check_landlock(env, &lsms),
        check_bpf_lsm(&lsms),
        check_userns(env),
        check_overlay_userns(env, version),
        check_sock_addr(version, cgroup_v2),
        check_sensord(env),
    ];

    let masked = apply_mask(&mut checks, env.mask.as_deref());
    let ok = |id: CheckId| checks.iter().any(|c| c.id == id && c.status == Status::Ok);
    // The Cell needs a user namespace, either directly or created by wyrd-sensord.
    let cell_mode = CheckId::ALL
        .iter()
        .filter(|c| c.required_for_cell())
        .all(|c| ok(*c))
        && (ok(CheckId::Userns) || ok(CheckId::Sensord));
    let kernel_capture = CheckId::ALL
        .iter()
        .filter(|c| c.required_for_capture())
        .all(|c| ok(*c));
    let kernel_network_enforcement = ok(CheckId::CgroupSockAddr) && ok(CheckId::CgroupV2);
    Report {
        kernel_release: release,
        checks,
        cell_mode,
        kernel_capture,
        kernel_network_enforcement,
        masked,
    }
}

fn apply_mask(checks: &mut [Check], mask: Option<&str>) -> Vec<String> {
    let Some(mask) = mask else { return Vec::new() };
    let mut masked = Vec::new();
    for name in mask.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(id) = CheckId::parse(name) {
            for c in checks.iter_mut().filter(|c| c.id == id) {
                c.status = Status::Missing;
                c.detail = format!("masked by WYRD_DOCTOR_MASK ({})", c.detail);
            }
            masked.push(name.to_string());
        }
    }
    masked
}

fn mk(
    id: CheckId,
    title: &'static str,
    status: Status,
    detail: impl Into<String>,
    consequence: &'static str,
) -> Check {
    Check {
        id,
        title,
        status,
        detail: detail.into(),
        consequence,
        required_for_cell: id.required_for_cell(),
        required_for_capture: id.required_for_capture(),
    }
}

fn check_kernel(release: &str, version: Option<(u32, u32, u32)>) -> Check {
    let (status, detail) = match at_least(version, 5, 15) {
        Some(true) => (Status::Ok, release.to_string()),
        Some(false) => (
            Status::Degraded,
            format!("{release} (5.15 or newer is supported)"),
        ),
        None => (
            Status::Unknown,
            "cannot read /proc/sys/kernel/osrelease".to_string(),
        ),
    };
    mk(
        CheckId::Kernel,
        "Kernel release",
        status,
        detail,
        "Older kernels are untested; some checks below may fail.",
    )
}

fn check_btf(env: &Env) -> Check {
    let present = env.exists("/sys/kernel/btf/vmlinux");
    mk(
        CheckId::Btf,
        "BTF type information",
        if present { Status::Ok } else { Status::Missing },
        if present {
            "/sys/kernel/btf/vmlinux present"
        } else {
            "/sys/kernel/btf/vmlinux missing"
        },
        "Without BTF the eBPF sensor cannot load (CO-RE); only intent capture works.",
    )
}

fn check_ringbuf(version: Option<(u32, u32, u32)>) -> Check {
    let (status, detail) = match at_least(version, 5, 8) {
        Some(true) => (Status::Ok, "BPF ring buffer available (kernel ≥ 5.8)"),
        Some(false) => (Status::Missing, "BPF ring buffer needs kernel ≥ 5.8"),
        None => (Status::Unknown, "kernel version unknown"),
    };
    mk(
        CheckId::Ringbuf,
        "BPF ring buffer",
        status,
        detail,
        "Without the ring buffer the sensor cannot stream effects.",
    )
}

fn check_cgroup_v2(present: bool) -> Check {
    mk(
        CheckId::CgroupV2,
        "cgroup v2 (unified hierarchy)",
        if present { Status::Ok } else { Status::Missing },
        if present {
            "/sys/fs/cgroup is cgroup2"
        } else {
            "cgroup v2 not mounted at /sys/fs/cgroup"
        },
        "Sessions cannot be scoped, frozen, or network-filtered.",
    )
}

fn check_delegation(env: &Env, cgroup_v2: bool) -> Check {
    let uid = env.probes.uid;
    let dir = format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service");
    let present = cgroup_v2 && env.exists(&dir);
    mk(
        CheckId::CgroupDelegation,
        "Delegated user cgroup",
        if present {
            Status::Ok
        } else {
            Status::Degraded
        },
        if present {
            format!("{dir} present (systemd user manager)")
        } else {
            format!("{dir} not found")
        },
        "Without delegation, wyrd-sensord must create session cgroups on the user's behalf.",
    )
}

fn check_landlock(env: &Env, lsms: &[String]) -> Check {
    let listed = lsms.iter().any(|l| l == "landlock");
    let (status, detail) = match (listed, env.probes.landlock_abi) {
        (true, Some(abi)) if abi >= 4 => (Status::Ok, format!("enabled, ABI v{abi}")),
        (true, Some(abi)) => (
            Status::Degraded,
            format!("enabled, ABI v{abi} (v4+ adds TCP port rules)"),
        ),
        (true, None) => (
            Status::Unknown,
            "listed in active LSMs, ABI probe failed".to_string(),
        ),
        (false, _) => (Status::Missing, "not in the active LSM list".to_string()),
    };
    mk(
        CheckId::Landlock,
        "Landlock LSM",
        status,
        detail,
        "Without Landlock the Cell cannot confine file access; only observe mode is possible.",
    )
}

fn check_bpf_lsm(lsms: &[String]) -> Check {
    let listed = lsms.iter().any(|l| l == "bpf");
    mk(
        CheckId::BpfLsm,
        "BPF LSM",
        if listed { Status::Ok } else { Status::Degraded },
        if listed {
            "bpf is in the active LSM list".to_string()
        } else {
            format!("not enabled (active: {})", lsms.join(","))
        },
        "Optional. Without it, exec denial is detect-and-stop and file checks rely on Landlock.",
    )
}

fn check_userns(env: &Env) -> Check {
    let max = env
        .read("/proc/sys/user/max_user_namespaces")
        .and_then(|s| s.parse::<u64>().ok());
    let debian_clone = env.read("/proc/sys/kernel/unprivileged_userns_clone");
    let apparmor = env.read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns");
    let (status, detail) = if max == Some(0) || debian_clone.as_deref() == Some("0") {
        (
            Status::Missing,
            "unprivileged user namespaces are disabled".to_string(),
        )
    } else if apparmor.as_deref() == Some("1") && env.exists("/etc/apparmor.d/wyrd") {
        (
            Status::Ok,
            "AppArmor restricts user namespaces; the wyrd profile allows them".to_string(),
        )
    } else if apparmor.as_deref() == Some("1") {
        (
            Status::Degraded,
            "restricted by AppArmor; install deploy/apparmor/wyrd or use wyrd-sensord mounts"
                .to_string(),
        )
    } else if max.is_some() {
        (
            Status::Ok,
            "unprivileged user namespaces allowed".to_string(),
        )
    } else {
        (
            Status::Unknown,
            "cannot read /proc/sys/user/max_user_namespaces".to_string(),
        )
    };
    mk(
        CheckId::Userns,
        "Unprivileged user namespaces",
        status,
        detail,
        "The Cell needs a user namespace, either directly or through wyrd-sensord.",
    )
}

fn check_overlay_userns(env: &Env, version: Option<(u32, u32, u32)>) -> Check {
    let fs_listed = env
        .read("/proc/filesystems")
        .is_some_and(|s| s.contains("overlay"));
    let module = env.exists("/sys/module/overlay");
    let new_enough = at_least(version, 5, 11);
    let (status, detail) = match (fs_listed || module, new_enough) {
        (true, Some(true)) => (
            Status::Ok,
            "overlayfs available, mountable in user namespaces",
        ),
        (true, Some(false)) => (
            Status::Missing,
            "overlay in user namespaces needs kernel ≥ 5.11",
        ),
        (false, _) => (Status::Missing, "overlayfs not available"),
        (true, None) => (Status::Unknown, "kernel version unknown"),
    };
    mk(
        CheckId::OverlayUserns,
        "overlayfs in user namespaces",
        status,
        detail,
        "Without overlayfs there are no checkpoints, rewind, or fork.",
    )
}

fn check_sock_addr(version: Option<(u32, u32, u32)>, cgroup_v2: bool) -> Check {
    let (status, detail) = match (at_least(version, 4, 17), cgroup_v2) {
        (Some(true), true) => (Status::Ok, "cgroup/connect4 and connect6 hooks available"),
        (Some(false), _) => (Status::Missing, "needs kernel ≥ 4.17"),
        (_, false) => (Status::Missing, "needs cgroup v2"),
        (None, _) => (Status::Unknown, "kernel version unknown"),
    };
    mk(
        CheckId::CgroupSockAddr,
        "cgroup socket-address hooks",
        status,
        detail,
        "Without them network egress cannot be filtered in the kernel.",
    )
}

fn check_sensord(env: &Env) -> Check {
    let path = env.root.join(&env.sensord_socket);
    let present = path.exists();
    let shown = Path::new("/").join(&env.sensord_socket);
    mk(
        CheckId::Sensord,
        "wyrd-sensord",
        if present { Status::Ok } else { Status::Missing },
        if present {
            format!("listening at {}", shown.display())
        } else {
            format!(
                "no socket at {} (run `make sensord-install`)",
                shown.display()
            )
        },
        "Without the sensor only intent (hooks, MCP) is captured, not kernel effects.",
    )
}

/// Render the report as an aligned text table.
pub fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Wyrdline doctor — kernel {}\n", report.kernel_release);
    for c in &report.checks {
        let mark = match c.status {
            Status::Ok => "✓",
            Status::Degraded => "!",
            Status::Missing => "✗",
            Status::Unknown => "?",
        };
        let req = if c.required_for_cell { " [cell]" } else { "" };
        let _ = writeln!(out, " {mark} {:<32} {}{req}", c.title, c.detail);
        if c.status != Status::Ok {
            let _ = writeln!(out, "   {:<32} → {}", "", c.consequence);
        }
    }
    let yn = |b: bool| if b { "yes" } else { "no" };
    let _ = writeln!(
        out,
        "\n Cell mode: {}   Kernel capture: {}   Kernel network enforcement: {}",
        yn(report.cell_mode),
        yn(report.kernel_capture),
        yn(report.kernel_network_enforcement)
    );
    if !report.masked.is_empty() {
        let _ = writeln!(out, " Masked: {}", report.masked.join(","));
    }
    out
}
