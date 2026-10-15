//! Raw event layout shared by `wyrd-ebpf` (kernel) and `wyrd-sensor`
//! (userspace). Plain `#[repr(C)]` data, no pointers, fixed size.
#![no_std]

/// Maximum bytes kept of one path (including the terminating NUL).
pub const PATH_LEN: usize = 256;
/// Exec: number of argv entries kept.
pub const MAX_ARGS: usize = 8;
/// Exec: bytes kept per argv entry.
pub const ARG_LEN: usize = 64;
/// Ancestor levels checked when matching a task's cgroup to a session.
pub const MAX_CGROUP_DEPTH: u32 = 16;

/// Event kinds.
pub mod kind {
    pub const EXEC: u32 = 1;
    pub const FORK: u32 = 2;
    pub const EXIT: u32 = 3;
    pub const OPEN: u32 = 4;
    pub const UNLINK: u32 = 5;
    pub const RENAME: u32 = 6;
    pub const MKDIR: u32 = 7;
    pub const CHMOD: u32 = 8;
    pub const TRUNCATE: u32 = 9;
    pub const CONNECT: u32 = 10;
    pub const EXIT_GROUP: u32 = 11;
    pub const CHDIR: u32 = 12;
    pub const FCHDIR: u32 = 13;
}

/// Flag bits in [`RawEvent::flags`].
pub mod flag {
    /// `s0` was cut at `PATH_LEN`.
    pub const S0_TRUNCATED: u16 = 1;
    /// `s1` was cut at `PATH_LEN`.
    pub const S1_TRUNCATED: u16 = 2;
    /// More than `MAX_ARGS` arguments or an argument was cut.
    pub const ARGV_TRUNCATED: u16 = 4;
    /// The thread is a thread-group leader exiting (`group_dead`).
    pub const GROUP_DEAD: u16 = 8;
}

/// Special directory fd meaning "relative to the current directory".
pub const AT_FDCWD: i64 = -100;

/// One kernel event. Field use per kind:
///
/// | kind | a0 | a1 | a2 | s0 | s1 |
/// |---|---|---|---|---|---|
/// | EXEC | | | | filename | |
/// | FORK | child tgid | | | | |
/// | EXIT | | | | | |
/// | EXIT_GROUP | exit code | | | | |
/// | OPEN | dfd | flags | mode | path | |
/// | UNLINK | dfd | flags | | path | |
/// | RENAME | old dfd | new dfd | flags | old path | new path |
/// | MKDIR | dfd | mode | | path | |
/// | CHMOD | dfd | mode | | path | |
/// | TRUNCATE | length | | | path | |
/// | CONNECT | family | port (host order) | | address bytes (4 or 16) | |
/// | CHDIR | | | | path | |
/// | FCHDIR | fd | | | | |
///
/// File operations, exec, and chdir are reported at syscall exit and only
/// when they succeeded; `ret` holds the return value (the new fd for opens).
/// CONNECT is reported at entry (an attempt), with `ret` = 0.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RawEvent {
    pub kind: u32,
    /// Thread-group id (the process id seen by users).
    pub tgid: u32,
    /// Thread id.
    pub tid: u32,
    /// Parent tgid (filled for FORK; 0 otherwise, resolved in userspace).
    pub ppid: u32,
    pub cgroup_id: u64,
    /// `bpf_ktime_get_ns` (CLOCK_MONOTONIC).
    pub ts_ns: u64,
    /// Start time of the process as recorded at fork/exec; 0 if unknown.
    pub start_ns: u64,
    pub a0: i64,
    pub a1: i64,
    pub a2: i64,
    pub ret: i64,
    pub len0: u16,
    pub len1: u16,
    pub argc: u16,
    pub flags: u16,
    pub comm: [u8; 16],
    pub s0: [u8; PATH_LEN],
    pub s1: [u8; PATH_LEN],
    pub argv: [[u8; ARG_LEN]; MAX_ARGS],
}

impl RawEvent {
    pub const ZERO: RawEvent = RawEvent {
        kind: 0,
        tgid: 0,
        tid: 0,
        ppid: 0,
        cgroup_id: 0,
        ts_ns: 0,
        start_ns: 0,
        a0: 0,
        a1: 0,
        a2: 0,
        ret: 0,
        len0: 0,
        len1: 0,
        argc: 0,
        flags: 0,
        comm: [0; 16],
        s0: [0; PATH_LEN],
        s1: [0; PATH_LEN],
        argv: [[0; ARG_LEN]; MAX_ARGS],
    };
}

/// Tracepoint field offsets that differ between kernels, read from tracefs
/// by userspace and written into the program's read-only data.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offsets {
    /// `task:task_newtask` field `pid` (the new task).
    pub newtask_pid: u32,
    /// `task:task_newtask` field `clone_flags`.
    pub newtask_clone_flags: u32,
    /// `sched:sched_process_exit` field `pid`.
    pub exit_pid: u32,
    /// `u32::MAX` when the kernel has no `group_dead` field.
    pub exit_group_dead: u32,
}

#[cfg(feature = "user")]
mod user {
    // SAFETY: plain-old-data with no padding-dependent invariants.
    unsafe impl aya::Pod for super::RawEvent {}
    // SAFETY: as above.
    unsafe impl aya::Pod for super::Offsets {}
}
