use aya_ebpf::helpers::{
    bpf_get_current_ancestor_cgroup_id, bpf_get_current_comm, bpf_get_current_pid_tgid,
    bpf_ktime_get_ns, bpf_probe_read_user, bpf_probe_read_user_str_bytes,
};
use aya_ebpf::macros::{cgroup_sock_addr, map, tracepoint};
use aya_ebpf::maps::lpm_trie::Key;
use aya_ebpf::maps::{Array, HashMap, LpmTrie, PerCpuArray, RingBuf};
use aya_ebpf::programs::{SockAddrContext, TracePointContext};
use wyrd_common::{
    ARG_LEN, Egress4, Egress6, MAX_ARGS, MAX_CGROUP_DEPTH, Offsets, PortKey, RawEvent, enforce,
    flag, kind, verdict,
};

/// Ring buffer to userspace (size overridden by the loader).
#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(1 << 20, 0);

/// Bit `l` is set when a registered session cgroup sits at depth `l` of the
/// hierarchy. Zero means nothing is registered: every program returns at
/// once, so unrelated processes pay almost nothing.
#[map]
static LEVELS: Array<u32> = Array::with_max_entries(1, 0);

/// Registered session cgroup ids → slot.
#[map]
static SESSIONS: HashMap<u64, u32> = HashMap::with_max_entries(1024, 0);

/// tgid → process start time (ns), recorded at process creation.
#[map]
static STARTS: HashMap<u32, u64> = HashMap::with_max_entries(65_536, 0);

/// tid → syscall in progress, emitted by `sys_exit_generic` when it succeeded.
#[map]
static PENDING: HashMap<u32, RawEvent> = HashMap::with_max_entries(8_192, 0);

/// Per-CPU scratch event: too large for the 512-byte BPF stack.
#[map]
static SCRATCH: PerCpuArray<RawEvent> = PerCpuArray::with_max_entries(1, 0);

/// Session cgroup id → `enforce::*` bits, for sessions whose egress is
/// enforced by the cgroup connect programs.
#[map]
static ENFORCE: HashMap<u64, u32> = HashMap::with_max_entries(1024, 0);

/// Allowed IPv4 prefixes per session (see `wyrd_common::Egress4`).
#[map]
static EGRESS4: LpmTrie<Egress4, u8> = LpmTrie::with_max_entries(65_536, 0);

/// Allowed IPv6 prefixes per session.
#[map]
static EGRESS6: LpmTrie<Egress6, u8> = LpmTrie::with_max_entries(65_536, 0);

/// Allowed destination ports per session.
#[map]
static PORTS: HashMap<PortKey, u8> = HashMap::with_max_entries(65_536, 0);

/// Per-CPU count of events lost because the ring buffer was full.
#[map]
static DROPS: PerCpuArray<u64> = PerCpuArray::with_max_entries(1, 0);

/// Layout-dependent tracepoint offsets, set by userspace before loading.
#[unsafe(no_mangle)]
static OFFSETS: Offsets = Offsets {
    newtask_pid: 8,
    newtask_clone_flags: 32,
    exit_pid: 24,
    exit_group_dead: 32,
};

const CLONE_THREAD: u64 = 0x0001_0000;
const EPERM: i64 = 1;
const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;

fn offsets() -> Offsets {
    // SAFETY: read-only global patched by the loader; volatile so the
    // compiler cannot constant-fold the default values.
    unsafe { core::ptr::read_volatile(&OFFSETS) }
}

/// Syscall argument `i` of a `sys_enter_*` tracepoint.
#[inline(always)]
fn arg(ctx: &TracePointContext, i: usize) -> u64 {
    // SAFETY: syscall tracepoints place args[6] at offset 16.
    unsafe { ctx.read_at::<u64>(16 + 8 * i) }.unwrap_or(0)
}

#[inline(always)]
fn session_cgroup() -> Option<u64> {
    let levels = LEVELS.get(0).copied().unwrap_or(0);
    if levels == 0 {
        return None;
    }
    // One helper call and one lookup per depth that has sessions (usually
    // exactly one), instead of walking every ancestor.
    let mut level = 0u32;
    while level < MAX_CGROUP_DEPTH {
        if levels & (1 << level) != 0 {
            // SAFETY: helper returns 0 past the task's own depth.
            let anc = unsafe { bpf_get_current_ancestor_cgroup_id(level as i32) };
            if anc != 0 && unsafe { SESSIONS.get(&anc) }.is_some() {
                return Some(anc);
            }
        }
        level += 1;
    }
    None
}

/// Fill the common header of the scratch event, or `None` when the task is
/// not part of a session.
#[inline(always)]
fn begin(k: u32) -> Option<&'static mut RawEvent> {
    let cg = session_cgroup()?;
    let ptr = SCRATCH.get_ptr_mut(0)?;
    // SAFETY: per-CPU slot, not shared with other CPUs; programs do not nest.
    let ev = unsafe { &mut *ptr };
    let pt = bpf_get_current_pid_tgid();
    let tgid = (pt >> 32) as u32;
    ev.kind = k;
    ev.tgid = tgid;
    ev.tid = pt as u32;
    ev.ppid = 0;
    ev.cgroup_id = cg;
    // SAFETY: helper call.
    ev.ts_ns = unsafe { bpf_ktime_get_ns() };
    ev.start_ns = unsafe { STARTS.get(&tgid) }.copied().unwrap_or(0);
    ev.a0 = 0;
    ev.a1 = 0;
    ev.a2 = 0;
    ev.ret = 0;
    ev.len0 = 0;
    ev.len1 = 0;
    ev.argc = 0;
    ev.flags = 0;
    ev.comm = bpf_get_current_comm().unwrap_or([0; 16]);
    Some(ev)
}

#[inline(always)]
fn read_s0(ev: &mut RawEvent, ptr: u64) {
    if ptr == 0 {
        return;
    }
    // SAFETY: bounded user-memory read into a fixed buffer.
    if let Ok(s) = unsafe { bpf_probe_read_user_str_bytes(ptr as *const u8, &mut ev.s0) } {
        ev.len0 = s.len() as u16;
        if s.len() >= ev.s0.len() - 1 {
            ev.flags |= flag::S0_TRUNCATED;
        }
    }
}

#[inline(always)]
fn read_s1(ev: &mut RawEvent, ptr: u64) {
    if ptr == 0 {
        return;
    }
    // SAFETY: as above.
    if let Ok(s) = unsafe { bpf_probe_read_user_str_bytes(ptr as *const u8, &mut ev.s1) } {
        ev.len1 = s.len() as u16;
        if s.len() >= ev.s1.len() - 1 {
            ev.flags |= flag::S1_TRUNCATED;
        }
    }
}

/// `BPF_RB_NO_WAKEUP`: do not wake the reader on every event (a wakeup per
/// syscall costs the traced process more than the capture itself); the
/// daemon drains the ring buffer on its own short poll interval.
const NO_WAKEUP: u64 = 1;

/// Bytes emitted per event: header + first path for most kinds, both paths
/// for renames, everything (argv) for exec. Userspace zero-fills the rest.
const ONE_PATH: usize = core::mem::offset_of!(RawEvent, s1);
const TWO_PATHS: usize = core::mem::offset_of!(RawEvent, argv);
const FULL: usize = core::mem::size_of::<RawEvent>();

#[inline(always)]
fn emit(ev: &RawEvent) {
    let p = ev as *const RawEvent;
    // SAFETY: each prefix lies within the struct; RawEvent is plain data.
    let r = match ev.kind {
        kind::EXEC => EVENTS.output::<[u8; FULL]>(unsafe { &*(p as *const [u8; FULL]) }, NO_WAKEUP),
        kind::RENAME => {
            EVENTS.output::<[u8; TWO_PATHS]>(unsafe { &*(p as *const [u8; TWO_PATHS]) }, NO_WAKEUP)
        }
        _ => EVENTS.output::<[u8; ONE_PATH]>(unsafe { &*(p as *const [u8; ONE_PATH]) }, NO_WAKEUP),
    };
    if r.is_err()
        && let Some(d) = DROPS.get_ptr_mut(0)
    {
        // SAFETY: per-CPU counter.
        unsafe { *d += 1 };
    }
}

const O_ACCMODE: i64 = 0o3;
const O_WRITE_INTENT: i64 = 0o100 | 0o1000 | 0o2000; // O_CREAT | O_TRUNC | O_APPEND
const O_DIRECTORY: i64 = 0o200000;

/// Read-only opens matter only for directories (to resolve later
/// `*at(dirfd, …)` paths) and for paths that may hold secrets: a
/// dot-component (`.env`, `.ssh/`, `.aws/`, …) or a key-file suffix.
#[inline(always)]
fn keep_read_open(ev: &RawEvent, flags: i64) -> bool {
    if flags & O_DIRECTORY != 0 {
        return true;
    }
    let len = (ev.len0 as usize).min(ev.s0.len());
    if len == 0 {
        return false;
    }
    if ev.s0[0] == b'.' {
        return true;
    }
    let mut i = 0usize;
    while i + 1 < len && i + 1 < ev.s0.len() {
        if ev.s0[i] == b'/' && ev.s0[i + 1] == b'.' {
            return true;
        }
        i += 1;
    }
    // len includes the NUL: the last real bytes are len-5 .. len-2.
    if len >= 5 && len <= ev.s0.len() {
        let t = [
            ev.s0[len - 5],
            ev.s0[len - 4],
            ev.s0[len - 3],
            ev.s0[len - 2],
        ];
        if &t == b".pem" || &t == b".key" || &t == b".p12" {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- processes

fn exec_enter(file: u64, argv: u64) -> u32 {
    let Some(ev) = begin(kind::EXEC) else {
        return 0;
    };
    read_s0(ev, file);
    if argv != 0 {
        let base = argv as *const u64;
        let mut i = 0usize;
        while i < MAX_ARGS {
            // SAFETY: reading the i-th argv pointer from user memory.
            let p = unsafe { bpf_probe_read_user(base.add(i)) }.unwrap_or(0);
            if p == 0 {
                break;
            }
            // SAFETY: bounded user string read.
            match unsafe { bpf_probe_read_user_str_bytes(p as *const u8, &mut ev.argv[i]) } {
                Ok(s) if s.len() >= ARG_LEN - 1 => ev.flags |= flag::ARGV_TRUNCATED,
                _ => {}
            }
            ev.argc += 1;
            i += 1;
        }
        if i == MAX_ARGS {
            // SAFETY: as above.
            let more = unsafe { bpf_probe_read_user(base.add(MAX_ARGS)) }.unwrap_or(0);
            if more != 0 {
                ev.flags |= flag::ARGV_TRUNCATED;
            }
        }
    }
    pend(ev);
    0
}

/// Park an event until its syscall returns.
#[inline(always)]
fn pend(ev: &mut RawEvent) {
    let tid = ev.tid;
    let _ = PENDING.insert(&tid, ev, 0);
}

/// Attached to the `sys_exit_*` tracepoint of every parked syscall:
/// emit the parked event when the call succeeded.
#[tracepoint]
pub fn sys_exit_generic(ctx: TracePointContext) -> u32 {
    let tid = bpf_get_current_pid_tgid() as u32;
    let Some(p) = PENDING.get_ptr_mut(&tid) else {
        return 0;
    };
    // SAFETY: sys_exit_* has `long ret` at offset 16.
    let ret = unsafe { ctx.read_at::<i64>(16) }.unwrap_or(-1);
    // SAFETY: map value owned by this tid.
    let connect = unsafe { (*p).kind } == kind::CONNECT;
    if connect {
        // Every attempt is reported, with its result. In an enforced
        // session the cgroup program is the only source of EPERM.
        // SAFETY: as above.
        let ev = unsafe { &mut *p };
        ev.ret = ret;
        if unsafe { ENFORCE.get(&ev.cgroup_id) }.is_some() {
            ev.a2 = if ret == -EPERM {
                verdict::DENIED
            } else {
                verdict::ALLOWED
            };
        }
        ev.ts_ns = unsafe { bpf_ktime_get_ns() };
        emit(ev);
    } else if ret >= 0 {
        // SAFETY: map value owned by this tid.
        let ev = unsafe { &mut *p };
        ev.ret = ret;
        ev.ts_ns = unsafe { bpf_ktime_get_ns() };
        if ev.kind == kind::EXEC {
            // The new image's name.
            ev.comm = bpf_get_current_comm().unwrap_or(ev.comm);
        }
        emit(ev);
    }
    let _ = PENDING.remove(&tid);
    0
}

#[tracepoint]
pub fn sys_enter_execve(ctx: TracePointContext) -> u32 {
    exec_enter(arg(&ctx, 0), arg(&ctx, 1))
}

#[tracepoint]
pub fn sys_enter_execveat(ctx: TracePointContext) -> u32 {
    exec_enter(arg(&ctx, 1), arg(&ctx, 2))
}

/// `task:task_newtask` runs in the parent; `clone_flags` tells threads
/// apart from new processes.
#[tracepoint]
pub fn task_newtask(ctx: TracePointContext) -> u32 {
    let o = offsets();
    // SAFETY: offsets come from this kernel's tracefs format.
    let child = unsafe { ctx.read_at::<i32>(o.newtask_pid as usize) }.unwrap_or(0) as u32;
    let flags = unsafe { ctx.read_at::<u64>(o.newtask_clone_flags as usize) }.unwrap_or(0);
    if child == 0 || flags & CLONE_THREAD != 0 {
        return 0;
    }
    let Some(ev) = begin(kind::FORK) else {
        return 0;
    };
    let now = ev.ts_ns;
    let _ = STARTS.insert(&child, &now, 0);
    ev.a0 = i64::from(child);
    ev.ppid = ev.tgid;
    emit(ev);
    0
}

#[tracepoint]
pub fn sched_process_exit(ctx: TracePointContext) -> u32 {
    let o = offsets();
    // SAFETY: offsets come from tracefs.
    let pid = unsafe { ctx.read_at::<i32>(o.exit_pid as usize) }.unwrap_or(0) as u32;
    let group_dead = if o.exit_group_dead == u32::MAX {
        let pt = bpf_get_current_pid_tgid();
        (pt >> 32) as u32 == pid
    } else {
        unsafe { ctx.read_at::<u8>(o.exit_group_dead as usize) }.unwrap_or(0) != 0
    };
    if !group_dead {
        return 0;
    }
    if let Some(ev) = begin(kind::EXIT) {
        ev.flags |= flag::GROUP_DEAD;
        emit(ev);
    }
    let tgid = (bpf_get_current_pid_tgid() >> 32) as u32;
    let _ = STARTS.remove(&tgid);
    0
}

#[tracepoint]
pub fn sys_enter_exit_group(ctx: TracePointContext) -> u32 {
    if let Some(ev) = begin(kind::EXIT_GROUP) {
        ev.a0 = arg(&ctx, 0) as i32 as i64;
        emit(ev);
    }
    0
}

// ---------------------------------------------------------------- files

fn path_event(k: u32, dfd: i64, path: u64, a1: i64, a2: i64) -> u32 {
    if let Some(ev) = begin(k) {
        ev.a0 = dfd;
        ev.a1 = a1;
        ev.a2 = a2;
        read_s0(ev, path);
        let read_only = k == kind::OPEN && a1 & O_ACCMODE == 0 && a1 & O_WRITE_INTENT == 0;
        if read_only && !keep_read_open(ev, a1) {
            return 0;
        }
        pend(ev);
    }
    0
}

const FDCWD: i64 = wyrd_common::AT_FDCWD;

#[tracepoint]
pub fn sys_enter_openat(ctx: TracePointContext) -> u32 {
    path_event(
        kind::OPEN,
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i64,
        arg(&ctx, 3) as i64,
    )
}

#[tracepoint]
pub fn sys_enter_openat2(ctx: TracePointContext) -> u32 {
    // struct open_how { u64 flags; u64 mode; u64 resolve; }
    let how = arg(&ctx, 2) as *const u64;
    // SAFETY: user pointer read.
    let flags = if how.is_null() {
        0
    } else {
        unsafe { bpf_probe_read_user(how) }.unwrap_or(0)
    };
    let mode = if how.is_null() {
        0
    } else {
        unsafe { bpf_probe_read_user(how.add(1)) }.unwrap_or(0)
    };
    path_event(
        kind::OPEN,
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        flags as i64,
        mode as i64,
    )
}

#[tracepoint]
pub fn sys_enter_open(ctx: TracePointContext) -> u32 {
    path_event(
        kind::OPEN,
        FDCWD,
        arg(&ctx, 0),
        arg(&ctx, 1) as i64,
        arg(&ctx, 2) as i64,
    )
}

#[tracepoint]
pub fn sys_enter_creat(ctx: TracePointContext) -> u32 {
    // creat(path, mode) == open(path, O_CREAT|O_WRONLY|O_TRUNC, mode)
    path_event(kind::OPEN, FDCWD, arg(&ctx, 0), 0o1101, arg(&ctx, 1) as i64)
}

#[tracepoint]
pub fn sys_enter_unlinkat(ctx: TracePointContext) -> u32 {
    path_event(
        kind::UNLINK,
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i64,
        0,
    )
}

#[tracepoint]
pub fn sys_enter_unlink(ctx: TracePointContext) -> u32 {
    path_event(kind::UNLINK, FDCWD, arg(&ctx, 0), 0, 0)
}

#[tracepoint]
pub fn sys_enter_rmdir(ctx: TracePointContext) -> u32 {
    // AT_REMOVEDIR
    path_event(kind::UNLINK, FDCWD, arg(&ctx, 0), 0x200, 0)
}

fn rename_event(olddfd: i64, old: u64, newdfd: i64, new: u64, flags: i64) -> u32 {
    if let Some(ev) = begin(kind::RENAME) {
        ev.a0 = olddfd;
        ev.a1 = newdfd;
        ev.a2 = flags;
        read_s0(ev, old);
        read_s1(ev, new);
        pend(ev);
    }
    0
}

#[tracepoint]
pub fn sys_enter_renameat2(ctx: TracePointContext) -> u32 {
    rename_event(
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i32 as i64,
        arg(&ctx, 3),
        arg(&ctx, 4) as i64,
    )
}

#[tracepoint]
pub fn sys_enter_renameat(ctx: TracePointContext) -> u32 {
    rename_event(
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i32 as i64,
        arg(&ctx, 3),
        0,
    )
}

#[tracepoint]
pub fn sys_enter_rename(ctx: TracePointContext) -> u32 {
    rename_event(FDCWD, arg(&ctx, 0), FDCWD, arg(&ctx, 1), 0)
}

#[tracepoint]
pub fn sys_enter_mkdirat(ctx: TracePointContext) -> u32 {
    path_event(
        kind::MKDIR,
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i64,
        0,
    )
}

#[tracepoint]
pub fn sys_enter_mkdir(ctx: TracePointContext) -> u32 {
    path_event(kind::MKDIR, FDCWD, arg(&ctx, 0), arg(&ctx, 1) as i64, 0)
}

#[tracepoint]
pub fn sys_enter_fchmodat(ctx: TracePointContext) -> u32 {
    path_event(
        kind::CHMOD,
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i64,
        0,
    )
}

#[tracepoint]
pub fn sys_enter_fchmodat2(ctx: TracePointContext) -> u32 {
    path_event(
        kind::CHMOD,
        arg(&ctx, 0) as i32 as i64,
        arg(&ctx, 1),
        arg(&ctx, 2) as i64,
        0,
    )
}

#[tracepoint]
pub fn sys_enter_chmod(ctx: TracePointContext) -> u32 {
    path_event(kind::CHMOD, FDCWD, arg(&ctx, 0), arg(&ctx, 1) as i64, 0)
}

#[tracepoint]
pub fn sys_enter_truncate(ctx: TracePointContext) -> u32 {
    // a0 carries the length for TRUNCATE (no dfd: path is cwd-relative).
    path_event(kind::TRUNCATE, arg(&ctx, 1) as i64, arg(&ctx, 0), 0, 0)
}

// ---------------------------------------------------------------- network

#[tracepoint]
pub fn sys_enter_connect(ctx: TracePointContext) -> u32 {
    let sa = arg(&ctx, 1) as *const u8;
    if sa.is_null() {
        return 0;
    }
    // SAFETY: user sockaddr reads with fixed offsets per family.
    let family = unsafe { bpf_probe_read_user(sa as *const u16) }.unwrap_or(0);
    if family != AF_INET && family != AF_INET6 {
        return 0;
    }
    let Some(ev) = begin(kind::CONNECT) else {
        return 0;
    };
    let port_be = unsafe { bpf_probe_read_user(sa.add(2) as *const u16) }.unwrap_or(0);
    ev.a0 = i64::from(family);
    ev.a1 = i64::from(u16::from_be(port_be));
    if family == AF_INET {
        let a = unsafe { bpf_probe_read_user(sa.add(4) as *const [u8; 4]) }.unwrap_or([0; 4]);
        ev.s0[..4].copy_from_slice(&a);
        ev.len0 = 4;
    } else {
        let a = unsafe { bpf_probe_read_user(sa.add(8) as *const [u8; 16]) }.unwrap_or([0; 16]);
        ev.s0[..16].copy_from_slice(&a);
        ev.len0 = 16;
    }
    ev.a2 = verdict::ATTEMPT;
    pend(ev);
    0
}

// ---------------------------------------------------------------- egress

#[inline(always)]
fn allowed4(cg: u64, a: [u8; 4]) -> bool {
    let mut d: Egress4 = [0; 12];
    d[..8].copy_from_slice(&cg.to_be_bytes());
    d[8..].copy_from_slice(&a);
    EGRESS4.get(&Key::new(96, d)).is_some()
}

#[inline(always)]
fn allowed6(cg: u64, a: [u8; 16]) -> bool {
    let mut d: Egress6 = [0; 24];
    d[..8].copy_from_slice(&cg.to_be_bytes());
    d[8..].copy_from_slice(&a);
    EGRESS6.get(&Key::new(192, d)).is_some()
}

/// 1 lets the connect proceed, 0 fails it with `EPERM`. Only registered
/// sessions with enforcement are checked; the outcome is reported by the
/// `connect` syscall tracepoints, which know the process.
#[inline(always)]
fn egress(family: u16, a4: [u8; 4], a6: [u8; 16], port: u16) -> i32 {
    let Some(cg) = session_cgroup() else {
        return 1;
    };
    // SAFETY: map lookup.
    let Some(flags) = (unsafe { ENFORCE.get(&cg) }).copied() else {
        return 1;
    };
    if flags & enforce::EGRESS != 0 {
        let mapped =
            family == AF_INET6 && a6[..10] == [0u8; 10] && a6[10] == 0xff && a6[11] == 0xff;
        let ok = if family == AF_INET {
            allowed4(cg, a4)
        } else if mapped {
            allowed4(cg, [a6[12], a6[13], a6[14], a6[15]])
        } else {
            allowed6(cg, a6)
        };
        if !ok {
            return 0;
        }
    }
    if flags & enforce::PORTS != 0 {
        let key = PortKey {
            cgroup_id: cg,
            port,
            _pad: [0; 6],
        };
        // SAFETY: map lookup.
        if unsafe { PORTS.get(&key) }.is_none() {
            return 0;
        }
    }
    1
}

#[cgroup_sock_addr(connect4)]
pub fn connect4(ctx: SockAddrContext) -> i32 {
    // SAFETY: the kernel passes a valid `bpf_sock_addr`.
    let (ip, port) = unsafe { ((*ctx.sock_addr).user_ip4, (*ctx.sock_addr).user_port) };
    // Both are in network byte order; the port in the low 16 bits.
    egress(
        AF_INET,
        ip.to_ne_bytes(),
        [0; 16],
        u16::from_be(port as u16),
    )
}

#[cgroup_sock_addr(connect6)]
pub fn connect6(ctx: SockAddrContext) -> i32 {
    // SAFETY: as above.
    let (w, port) = unsafe { ((*ctx.sock_addr).user_ip6, (*ctx.sock_addr).user_port) };
    let mut a = [0u8; 16];
    a[..4].copy_from_slice(&w[0].to_ne_bytes());
    a[4..8].copy_from_slice(&w[1].to_ne_bytes());
    a[8..12].copy_from_slice(&w[2].to_ne_bytes());
    a[12..].copy_from_slice(&w[3].to_ne_bytes());
    egress(AF_INET6, [0; 4], a, u16::from_be(port as u16))
}

// ---------------------------------------------------------------- cwd

#[tracepoint]
pub fn sys_enter_chdir(ctx: TracePointContext) -> u32 {
    path_event(kind::CHDIR, FDCWD, arg(&ctx, 0), 0, 0)
}

#[tracepoint]
pub fn sys_enter_fchdir(ctx: TracePointContext) -> u32 {
    if let Some(ev) = begin(kind::FCHDIR) {
        ev.a0 = arg(&ctx, 0) as i32 as i64;
        pend(ev);
    }
    0
}
