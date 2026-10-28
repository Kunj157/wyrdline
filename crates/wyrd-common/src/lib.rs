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

/// Syscalls the `sys_enter` program handles: the values of its `SYSCALLS`
/// map, indexed by this architecture's syscall number (0 = not handled).
/// Userspace fills the map, so the object stays architecture-neutral.
pub mod sys {
    pub const EXECVE: u32 = 1;
    pub const EXECVEAT: u32 = 2;
    pub const EXIT_GROUP: u32 = 3;
    pub const OPENAT: u32 = 4;
    pub const OPENAT2: u32 = 5;
    pub const OPEN: u32 = 6;
    pub const CREAT: u32 = 7;
    pub const UNLINKAT: u32 = 8;
    pub const UNLINK: u32 = 9;
    pub const RMDIR: u32 = 10;
    pub const RENAMEAT2: u32 = 11;
    pub const RENAMEAT: u32 = 12;
    pub const RENAME: u32 = 13;
    pub const MKDIRAT: u32 = 14;
    pub const MKDIR: u32 = 15;
    pub const FCHMODAT: u32 = 16;
    pub const FCHMODAT2: u32 = 17;
    pub const CHMOD: u32 = 18;
    pub const TRUNCATE: u32 = 19;
    pub const CONNECT: u32 = 20;
    pub const CHDIR: u32 = 21;
    pub const FCHDIR: u32 = 22;
}

/// Size of the `SYSCALLS` map: syscall numbers at or above it are ignored.
pub const MAX_SYSCALL_NR: u32 = 1024;

/// Verdict carried in `a2` of a CONNECT event.
pub mod verdict {
    /// Seen at syscall entry, no enforcement for this session.
    pub const ATTEMPT: i64 = 0;
    /// Checked by the cgroup connect program and allowed.
    pub const ALLOWED: i64 = 1;
    /// Checked by the cgroup connect program and refused (`EPERM`).
    pub const DENIED: i64 = 2;
}

/// Bits in the `ENFORCE` map value of a session cgroup.
pub mod enforce {
    /// Destinations must match the session's prefixes.
    pub const EGRESS: u32 = 1;
    /// Ports must be in the session's port set.
    pub const PORTS: u32 = 2;
}

/// LPM-trie key data for an IPv4 prefix of a session: the session cgroup
/// id (big endian, always matched in full) followed by the address.
/// The trie's prefix length is `64 + prefix`.
pub type Egress4 = [u8; 12];
/// As [`Egress4`] for IPv6 (16-byte address).
pub type Egress6 = [u8; 24];

/// Key of the per-session allowed-port set.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortKey {
    pub cgroup_id: u64,
    /// Host byte order.
    pub port: u16,
    pub _pad: [u8; 6],
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
/// | CONNECT | family | port (host order) | verdict (see [`verdict`]) | address bytes (4 or 16) | |
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

/// Layout facts that differ between kernels or architectures, filled by
/// userspace (tracefs formats, the architecture's `pt_regs`) and written
/// into the program's read-only data.
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
    /// Offsets of the six syscall arguments in the `struct pt_regs` that
    /// the raw `sys_enter` tracepoint passes.
    pub regs_args: [u32; 6],
    /// Offset in `task_struct` of the word that marks a compat (32-bit)
    /// syscall, whose numbers come from another table; `u32::MAX` when
    /// the architecture has none.
    pub compat_offset: u32,
    /// Bit of that word that is set during a compat syscall.
    pub compat_mask: u32,
}

// Padding checks for the types that cross the kernel boundary (`aya::Pod`).
// A type has no implicit padding exactly when its size equals the sum of its
// field sizes, because fields never overlap. Each assertion lists the fields
// in declaration order; adding or resizing a field without updating its
// assertion fails the build, so a padding hole cannot appear unnoticed.
const _: () =
    assert!(size_of::<PortKey>() == size_of::<u64>() + size_of::<u16>() + size_of::<[u8; 6]>());
const _: () = assert!(
    size_of::<Offsets>() == 4 * size_of::<u32>() + size_of::<[u32; 6]>() + 2 * size_of::<u32>()
);
const _: () = assert!(
    size_of::<RawEvent>()
        == 4 * size_of::<u32>() // kind, tgid, tid, ppid
            + 3 * size_of::<u64>() // cgroup_id, ts_ns, start_ns
            + 4 * size_of::<i64>() // a0, a1, a2, ret
            + 4 * size_of::<u16>() // len0, len1, argc, flags
            + size_of::<[u8; 16]>() // comm
            + 2 * size_of::<[u8; PATH_LEN]>() // s0, s1
            + size_of::<[[u8; ARG_LEN]; MAX_ARGS]>() // argv
);

#[cfg(feature = "user")]
mod user {
    // SAFETY: `RawEvent`, `Offsets` and `PortKey` are `#[repr(C)]`, made only
    // of integers and byte arrays, so every bit pattern is a valid value and
    // none of them has implicit padding (checked at compile time above).
    unsafe impl aya::Pod for super::RawEvent {}
    // SAFETY: as above.
    unsafe impl aya::Pod for super::Offsets {}
    // SAFETY: as above; the explicit `_pad` field is part of the size sum.
    unsafe impl aya::Pod for super::PortKey {}
}
