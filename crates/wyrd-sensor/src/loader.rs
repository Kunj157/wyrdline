//! Load, attach, register cgroups, read events.

use std::os::fd::AsFd as _;
use std::time::Duration;

use aya::maps::{HashMap, MapData, PerCpuArray, RingBuf};
use aya::programs::TracePoint;
use aya::{Ebpf, EbpfLoader};
use wyrd_common::RawEvent;

use crate::{EBPF_OBJECT, Result, SensorError, tracefs};

/// Entry programs and the tracepoint each attaches to. Missing tracepoints
/// (for example legacy syscalls that do not exist on aarch64) are skipped.
pub const PROGRAMS: &[(&str, &str)] = &[
    ("sys_enter_execve", "syscalls"),
    ("sys_enter_execveat", "syscalls"),
    ("task_newtask", "task"),
    ("sched_process_exit", "sched"),
    ("sys_enter_exit_group", "syscalls"),
    ("sys_enter_openat", "syscalls"),
    ("sys_enter_openat2", "syscalls"),
    ("sys_enter_open", "syscalls"),
    ("sys_enter_creat", "syscalls"),
    ("sys_enter_unlinkat", "syscalls"),
    ("sys_enter_unlink", "syscalls"),
    ("sys_enter_rmdir", "syscalls"),
    ("sys_enter_renameat2", "syscalls"),
    ("sys_enter_renameat", "syscalls"),
    ("sys_enter_rename", "syscalls"),
    ("sys_enter_mkdirat", "syscalls"),
    ("sys_enter_mkdir", "syscalls"),
    ("sys_enter_fchmodat", "syscalls"),
    ("sys_enter_fchmodat2", "syscalls"),
    ("sys_enter_chmod", "syscalls"),
    ("sys_enter_truncate", "syscalls"),
    ("sys_enter_connect", "syscalls"),
    ("sys_enter_chdir", "syscalls"),
    ("sys_enter_fchdir", "syscalls"),
];

/// Syscalls whose entry program parks an event; `sys_exit_generic` is
/// attached to each `sys_exit_<name>` to emit it on success.
pub const PARKED: &[&str] = &[
    "execve",
    "execveat",
    "openat",
    "openat2",
    "open",
    "creat",
    "unlinkat",
    "unlink",
    "rmdir",
    "renameat2",
    "renameat",
    "rename",
    "mkdirat",
    "mkdir",
    "fchmodat",
    "fchmodat2",
    "chmod",
    "truncate",
    "chdir",
    "fchdir",
];

#[derive(Debug, Clone)]
pub struct SensorConfig {
    /// Ring buffer size in bytes (power of two, multiple of the page size).
    pub ringbuf_bytes: u32,
}

impl Default for SensorConfig {
    fn default() -> Self {
        SensorConfig {
            ringbuf_bytes: 64 << 20,
        }
    }
}

/// What was attached.
#[derive(Debug, Clone, Default)]
pub struct LoadReport {
    pub attached: Vec<String>,
    /// Programs whose tracepoint does not exist on this kernel.
    pub skipped: Vec<(String, String)>,
}

/// A loaded sensor.
pub struct Sensor {
    ebpf: Ebpf,
    ring: RingBuf<MapData>,
    /// Registered sessions per cgroup depth.
    levels: std::collections::BTreeMap<u32, u32>,
}

/// Depth of a cgroup directory below the cgroup2 mount (`/sys/fs/cgroup` = 0).
pub fn cgroup_level(path: &std::path::Path) -> u32 {
    path.strip_prefix("/sys/fs/cgroup").map_or(u32::MAX, |rel| {
        u32::try_from(rel.components().count()).unwrap_or(u32::MAX)
    })
}

impl std::fmt::Debug for Sensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sensor")
    }
}

fn map_err(e: impl std::fmt::Display) -> SensorError {
    SensorError::Map(e.to_string())
}

impl Sensor {
    /// Load and attach every program. Needs `CAP_BPF` + `CAP_PERFMON` (or root).
    pub fn load(cfg: &SensorConfig) -> Result<(Sensor, LoadReport)> {
        let offsets = tracefs::read_offsets(&tracefs::root()?)?;
        let mut ebpf = EbpfLoader::new()
            .override_global("OFFSETS", &offsets, true)
            .map_max_entries("EVENTS", cfg.ringbuf_bytes)
            .load(EBPF_OBJECT)
            .map_err(|e| SensorError::Load(e.to_string()))?;
        let mut report = LoadReport::default();
        for (name, category) in PROGRAMS {
            let prog: &mut TracePoint = ebpf
                .program_mut(name)
                .ok_or_else(|| SensorError::Load(format!("program {name} missing from object")))?
                .try_into()
                .map_err(|e| SensorError::Load(format!("{name}: {e}")))?;
            prog.load()
                .map_err(|e| SensorError::Load(format!("{name}: verifier: {e}")))?;
            match prog.attach(category, name) {
                Ok(_) => report.attached.push((*name).to_string()),
                Err(e) => report.skipped.push(((*name).to_string(), e.to_string())),
            }
        }
        let exit: &mut TracePoint = ebpf
            .program_mut("sys_exit_generic")
            .ok_or_else(|| SensorError::Load("program sys_exit_generic missing".into()))?
            .try_into()
            .map_err(|e| SensorError::Load(format!("sys_exit_generic: {e}")))?;
        exit.load()
            .map_err(|e| SensorError::Load(format!("sys_exit_generic: verifier: {e}")))?;
        for name in PARKED {
            let tp = format!("sys_exit_{name}");
            match exit.attach("syscalls", &tp) {
                Ok(_) => report.attached.push(tp),
                Err(e) => report.skipped.push((tp, e.to_string())),
            }
        }
        let ring = RingBuf::try_from(
            ebpf.take_map("EVENTS")
                .ok_or_else(|| map_err("EVENTS missing"))?,
        )
        .map_err(map_err)?;
        Ok((
            Sensor {
                ebpf,
                ring,
                levels: std::collections::BTreeMap::new(),
            },
            report,
        ))
    }

    /// Start streaming events of a cgroup (and its descendants).
    /// `level` is the cgroup's depth below the cgroup2 root (see
    /// [`cgroup_level`]).
    pub fn register(&mut self, cgroup_id: u64, level: u32) -> Result<()> {
        if level >= wyrd_common::MAX_CGROUP_DEPTH {
            return Err(map_err(format!(
                "cgroup depth {level} exceeds {}",
                wyrd_common::MAX_CGROUP_DEPTH
            )));
        }
        let mut m: HashMap<_, u64, u32> = HashMap::try_from(
            self.ebpf
                .map_mut("SESSIONS")
                .ok_or_else(|| map_err("SESSIONS missing"))?,
        )
        .map_err(map_err)?;
        m.insert(cgroup_id, level, 0).map_err(map_err)?;
        *self.levels.entry(level).or_insert(0) += 1;
        self.sync_levels()
    }

    fn sync_levels(&mut self) -> Result<()> {
        let mask = self
            .levels
            .iter()
            .filter(|(_, n)| **n > 0)
            .fold(0u32, |m, (l, _)| m | (1 << l));
        let mut a: aya::maps::Array<_, u32> = aya::maps::Array::try_from(
            self.ebpf
                .map_mut("LEVELS")
                .ok_or_else(|| map_err("LEVELS missing"))?,
        )
        .map_err(map_err)?;
        a.set(0, mask, 0).map_err(map_err)
    }

    pub fn unregister(&mut self, cgroup_id: u64) -> Result<()> {
        let mut m: HashMap<_, u64, u32> = HashMap::try_from(
            self.ebpf
                .map_mut("SESSIONS")
                .ok_or_else(|| map_err("SESSIONS missing"))?,
        )
        .map_err(map_err)?;
        let level = m.get(&cgroup_id, 0).ok();
        match m.remove(&cgroup_id) {
            Ok(()) | Err(aya::maps::MapError::KeyNotFound) => {}
            Err(e) => return Err(map_err(e)),
        }
        if let Some(l) = level
            && let Some(n) = self.levels.get_mut(&l)
        {
            *n = n.saturating_sub(1);
        }
        self.sync_levels()
    }

    /// Total events dropped because the ring buffer was full.
    pub fn drops(&self) -> Result<u64> {
        let a: PerCpuArray<_, u64> = PerCpuArray::try_from(
            self.ebpf
                .map("DROPS")
                .ok_or_else(|| map_err("DROPS missing"))?,
        )
        .map_err(map_err)?;
        Ok(a.get(&0, 0).map_err(map_err)?.iter().sum())
    }

    /// Return everything available, waiting up to `timeout` when the buffer
    /// is empty. Programs emit without wakeups, so callers should poll with
    /// short timeouts (the daemon uses 10 ms).
    pub fn poll(&mut self, timeout: Duration) -> Result<Vec<RawEvent>> {
        let mut out = Vec::new();
        self.drain(&mut out);
        if out.is_empty() {
            let fd = self.ring.as_fd();
            let mut fds = [rustix::event::PollFd::new(
                &fd,
                rustix::event::PollFlags::IN,
            )];
            let ts = rustix::time::Timespec {
                tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: i64::from(timeout.subsec_nanos()),
            };
            match rustix::event::poll(&mut fds, Some(&ts)) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(SensorError::Io(e.into())),
            }
            self.drain(&mut out);
        }
        Ok(out)
    }

    fn drain(&mut self, out: &mut Vec<RawEvent>) {
        let header = std::mem::offset_of!(RawEvent, s0);
        while let Some(item) = self.ring.next() {
            if item.len() < header {
                continue;
            }
            // Events are emitted as size-tiered prefixes; the rest stays zero.
            let mut ev = RawEvent::ZERO;
            let n = item.len().min(std::mem::size_of::<RawEvent>());
            #[allow(unsafe_code)]
            // SAFETY: RawEvent is plain data; `n` never exceeds its size.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    item.as_ptr(),
                    std::ptr::from_mut(&mut ev).cast::<u8>(),
                    n,
                );
            }
            out.push(ev);
        }
    }
}
