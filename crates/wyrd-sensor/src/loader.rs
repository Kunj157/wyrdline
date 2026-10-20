//! Load, attach, register cgroups, read events.

use std::os::fd::AsFd as _;
use std::time::Duration;

use aya::maps::lpm_trie::{Key, LpmTrie};
use aya::maps::{HashMap, MapData, PerCpuArray, RingBuf};
use aya::programs::cgroup_sock_addr::CgroupSockAddrLinkId;
use aya::programs::{CgroupAttachMode, CgroupSockAddr, RawTracePoint, TracePoint};
use aya::{Ebpf, EbpfLoader};
use wyrd_common::RawEvent;

use crate::{EBPF_OBJECT, Result, SensorError, tracefs};

/// Scheduler tracepoint programs and their category.
pub const PROGRAMS: &[(&str, &str)] = &[("task_newtask", "task"), ("sched_process_exit", "sched")];

/// Raw tracepoint programs, each attached to the tracepoint of its name:
/// every handled syscall goes through these two (see [`crate::syscalls`]).
pub const RAW_PROGRAMS: &[&str] = &["sys_enter", "sys_exit"];

/// The cgroup connect programs, attached per enforced session.
const CONNECT_PROGRAMS: &[&str] = &["connect4", "connect6"];

/// Egress rules of one session, enforced in the kernel.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EgressRules {
    /// Allowed destination prefixes (`10.0.0.0/8`, `::1/128`, bare
    /// addresses); empty with `restrict_hosts` means no destination at all.
    pub cidrs: Vec<String>,
    /// Whether destinations are restricted to `cidrs`.
    pub restrict_hosts: bool,
    /// Allowed TCP/UDP destination ports; empty means every port.
    pub ports: Vec<u16>,
}

/// Parse a prefix into (address bytes, prefix length).
pub fn parse_prefix(s: &str) -> Option<(std::net::IpAddr, u32)> {
    let (a, p) = s.split_once('/').unwrap_or((s, ""));
    let addr: std::net::IpAddr = a.trim().parse().ok()?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = if p.is_empty() {
        max
    } else {
        p.trim().parse().ok()?
    };
    (prefix <= max).then_some((addr, prefix))
}

/// Kernel state of one enforced session, for replacement and cleanup.
#[derive(Debug, Default)]
struct Enforced {
    links: Vec<(&'static str, CgroupSockAddrLinkId)>,
    /// (prefix length, key data) of every inserted prefix.
    v4: Vec<(u32, wyrd_common::Egress4)>,
    v6: Vec<(u32, wyrd_common::Egress6)>,
    ports: Vec<wyrd_common::PortKey>,
}

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

/// Kernel counters of one loaded program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramStats {
    pub name: String,
    pub run_cnt: u64,
    pub misses: u64,
}

/// A numeric `key:\tvalue` line of a BPF program's fdinfo (0 when absent:
/// older kernels do not report every counter).
pub fn fdinfo_field(info: &str, key: &str) -> u64 {
    info.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0)
}

/// A loaded sensor.
pub struct Sensor {
    ebpf: Ebpf,
    ring: RingBuf<MapData>,
    /// Registered sessions per cgroup depth.
    levels: std::collections::BTreeMap<u32, u32>,
    enforced: std::collections::HashMap<u64, Enforced>,
    /// Why the connect programs could not be loaded, if they could not.
    connect_unavailable: Option<String>,
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
        let arch = crate::syscalls::host()
            .ok_or_else(|| SensorError::Load("unsupported architecture".into()))?;
        let mut offsets = tracefs::read_offsets(&tracefs::root()?)?;
        offsets.regs_args = arch.regs_args;
        offsets.compat_offset = arch.compat_offset;
        offsets.compat_mask = arch.compat_mask;
        let mut ebpf = EbpfLoader::new()
            .override_global("OFFSETS", &offsets, true)
            .map_max_entries("EVENTS", cfg.ringbuf_bytes)
            .load(EBPF_OBJECT)
            .map_err(|e| SensorError::Load(e.to_string()))?;
        {
            let mut table: aya::maps::Array<_, u32> = aya::maps::Array::try_from(
                ebpf.map_mut("SYSCALLS")
                    .ok_or_else(|| map_err("SYSCALLS missing"))?,
            )
            .map_err(map_err)?;
            for (nr, code) in arch.table {
                table.set(*nr, code, 0).map_err(map_err)?;
            }
        }
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
        // Without these two nothing of the agent's syscalls is seen: a
        // failure is a load error, not a skipped program.
        for name in RAW_PROGRAMS {
            let prog: &mut RawTracePoint = ebpf
                .program_mut(name)
                .ok_or_else(|| SensorError::Load(format!("program {name} missing from object")))?
                .try_into()
                .map_err(|e| SensorError::Load(format!("{name}: {e}")))?;
            prog.load()
                .map_err(|e| SensorError::Load(format!("{name}: verifier: {e}")))?;
            prog.attach(name)
                .map_err(|e| SensorError::Load(format!("attach {name}: {e}")))?;
            report.attached.push(format!("raw_tp/{name}"));
        }
        // The connect programs are loaded now (verifier) but attached only
        // to the cgroups of sessions whose egress is enforced.
        let mut connect_unavailable = None;
        for name in CONNECT_PROGRAMS {
            let loaded = ebpf
                .program_mut(name)
                .ok_or_else(|| format!("program {name} missing from object"))
                .and_then(|p| {
                    let p: &mut CgroupSockAddr =
                        p.try_into().map_err(|e| format!("{name}: {e}"))?;
                    p.load().map_err(|e| format!("{name}: verifier: {e}"))
                });
            match loaded {
                Ok(()) => report.attached.push(format!("{name} (per session)")),
                Err(e) => {
                    report.skipped.push(((*name).to_string(), e.clone()));
                    connect_unavailable.get_or_insert(e);
                }
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
                enforced: std::collections::HashMap::new(),
                connect_unavailable,
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

    /// Enforce `rules` for a registered session cgroup (`dir` is its
    /// directory): attach the connect programs once, then replace the
    /// session's prefixes and ports.
    pub fn enforce(
        &mut self,
        cgroup_id: u64,
        dir: &std::path::Path,
        rules: &EgressRules,
    ) -> Result<()> {
        if let Some(why) = &self.connect_unavailable {
            return Err(SensorError::Load(format!(
                "egress enforcement unavailable: {why}"
            )));
        }
        let mut state = self.enforced.remove(&cgroup_id).unwrap_or_default();
        if state.links.is_empty() {
            let cgroup = std::fs::File::open(dir)?;
            for name in CONNECT_PROGRAMS {
                let p: &mut CgroupSockAddr = self
                    .ebpf
                    .program_mut(name)
                    .ok_or_else(|| SensorError::Load(format!("{name} missing")))?
                    .try_into()
                    .map_err(|e| SensorError::Load(format!("{name}: {e}")))?;
                // On 5.7+ this creates a bpf_link, which always coexists
                // with other programs on the cgroup; the kernel rejects
                // BPF_F_ALLOW_MULTI on link creation.
                let link = p
                    .attach(cgroup.as_fd(), CgroupAttachMode::Single)
                    .map_err(|e| {
                        let cause = std::error::Error::source(&e)
                            .map(|s| format!(": {s}"))
                            .unwrap_or_default();
                        SensorError::Load(format!("attach {name}: {e}{cause}"))
                    })?;
                state.links.push((name, link));
            }
        }
        let r = self.replace_rules(cgroup_id, &mut state, rules);
        self.enforced.insert(cgroup_id, state);
        r
    }

    fn replace_rules(&mut self, cg: u64, state: &mut Enforced, rules: &EgressRules) -> Result<()> {
        let mut v4: Vec<(u32, wyrd_common::Egress4)> = Vec::new();
        let mut v6: Vec<(u32, wyrd_common::Egress6)> = Vec::new();
        for c in &rules.cidrs {
            let (addr, prefix) =
                parse_prefix(c).ok_or_else(|| map_err(format!("{c} is not a prefix")))?;
            match addr {
                std::net::IpAddr::V4(a) => {
                    let mut d = [0u8; 12];
                    d[..8].copy_from_slice(&cg.to_be_bytes());
                    d[8..].copy_from_slice(&a.octets());
                    v4.push((64 + prefix, d));
                }
                std::net::IpAddr::V6(a) => {
                    let mut d = [0u8; 24];
                    d[..8].copy_from_slice(&cg.to_be_bytes());
                    d[8..].copy_from_slice(&a.octets());
                    v6.push((64 + prefix, d));
                }
            }
        }
        let ports: Vec<wyrd_common::PortKey> = rules
            .ports
            .iter()
            .map(|p| wyrd_common::PortKey {
                cgroup_id: cg,
                port: *p,
                _pad: [0; 6],
            })
            .collect();
        {
            let mut t4: LpmTrie<_, wyrd_common::Egress4, u8> = LpmTrie::try_from(
                self.ebpf
                    .map_mut("EGRESS4")
                    .ok_or_else(|| map_err("EGRESS4 missing"))?,
            )
            .map_err(map_err)?;
            for (p, d) in &v4 {
                t4.insert(&Key::new(*p, *d), 1, 0).map_err(map_err)?;
            }
            for (p, d) in state.v4.iter().filter(|k| !v4.contains(k)) {
                let _ = t4.remove(&Key::new(*p, *d));
            }
        }
        {
            let mut t6: LpmTrie<_, wyrd_common::Egress6, u8> = LpmTrie::try_from(
                self.ebpf
                    .map_mut("EGRESS6")
                    .ok_or_else(|| map_err("EGRESS6 missing"))?,
            )
            .map_err(map_err)?;
            for (p, d) in &v6 {
                t6.insert(&Key::new(*p, *d), 1, 0).map_err(map_err)?;
            }
            for (p, d) in state.v6.iter().filter(|k| !v6.contains(k)) {
                let _ = t6.remove(&Key::new(*p, *d));
            }
        }
        {
            let mut pm: HashMap<_, wyrd_common::PortKey, u8> = HashMap::try_from(
                self.ebpf
                    .map_mut("PORTS")
                    .ok_or_else(|| map_err("PORTS missing"))?,
            )
            .map_err(map_err)?;
            for k in &ports {
                pm.insert(k, 1, 0).map_err(map_err)?;
            }
            for k in state.ports.iter().filter(|k| !ports.contains(k)) {
                let _ = pm.remove(k);
            }
        }
        let flags = if rules.restrict_hosts {
            wyrd_common::enforce::EGRESS
        } else {
            0
        } | if rules.ports.is_empty() {
            0
        } else {
            wyrd_common::enforce::PORTS
        };
        let mut en: HashMap<_, u64, u32> = HashMap::try_from(
            self.ebpf
                .map_mut("ENFORCE")
                .ok_or_else(|| map_err("ENFORCE missing"))?,
        )
        .map_err(map_err)?;
        en.insert(cg, flags, 0).map_err(map_err)?;
        state.v4 = v4;
        state.v6 = v6;
        state.ports = ports;
        Ok(())
    }

    fn release_enforcement(&mut self, cg: u64) {
        let Some(mut state) = self.enforced.remove(&cg) else {
            return;
        };
        let _ = self.replace_rules(cg, &mut state, &EgressRules::default());
        if let Some(m) = self.ebpf.map_mut("ENFORCE")
            && let Ok(mut en) = HashMap::<_, u64, u32>::try_from(m)
        {
            let _ = en.remove(&cg);
        }
        for (name, link) in state.links {
            if let Some(p) = self.ebpf.program_mut(name)
                && let Ok(p) = <&mut CgroupSockAddr>::try_from(p)
            {
                let _ = p.detach(link);
            }
        }
    }

    pub fn unregister(&mut self, cgroup_id: u64) -> Result<()> {
        self.release_enforcement(cgroup_id);
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

    /// Total events lost in the kernel: ring buffer full, syscalls that
    /// could not be parked, and program runs the kernel skipped (recursion
    /// misses, e.g. a program interrupted by another on the same CPU).
    pub fn drops(&self) -> Result<u64> {
        let a: PerCpuArray<_, u64> = PerCpuArray::try_from(
            self.ebpf
                .map("DROPS")
                .ok_or_else(|| map_err("DROPS missing"))?,
        )
        .map_err(map_err)?;
        let counted: u64 = a.get(&0, 0).map_err(map_err)?.iter().sum();
        let missed: u64 = self.program_stats().iter().map(|p| p.misses).sum();
        Ok(counted + missed)
    }

    /// Per-program counters from the kernel (`/proc/self/fdinfo`): runs
    /// (counted only while BPF stats are enabled) and recursion misses.
    pub fn program_stats(&self) -> Vec<ProgramStats> {
        use std::os::fd::AsRawFd as _;
        self.ebpf
            .programs()
            .filter_map(|(name, p)| {
                let fd = p.fd().ok()?.as_fd().as_raw_fd();
                let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).ok()?;
                Some(ProgramStats {
                    name: name.to_string(),
                    run_cnt: fdinfo_field(&info, "run_cnt"),
                    misses: fdinfo_field(&info, "recursion_misses"),
                })
            })
            .collect()
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
