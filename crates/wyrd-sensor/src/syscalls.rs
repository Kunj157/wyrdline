//! What the raw `sys_enter` program needs to know about this architecture:
//! which syscall numbers to handle, where `struct pt_regs` keeps the
//! arguments, and how a compat (32-bit) syscall is recognised.
//!
//! The per-syscall `syscalls:sys_enter_*` tracepoints are not used: on 7.0
//! kernels the perf path behind them runs BPF programs only after copying
//! the syscall's user-space strings for its own event record, and most
//! calls of path-taking syscalls never reach the program (no drop, no
//! recursion miss is counted). The raw `sys_enter`/`sys_exit` tracepoints
//! call the program directly on every kernel.

use wyrd_common::sys;

/// Layout facts of one architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arch {
    pub name: &'static str,
    /// (syscall number, `wyrd_common::sys` code).
    pub table: &'static [(u32, u32)],
    /// Offsets of the six syscall arguments in `struct pt_regs`.
    pub regs_args: [u32; 6],
    /// Offset in `task_struct` of the compat marker word, or `u32::MAX`.
    pub compat_offset: u32,
    pub compat_mask: u32,
}

/// `x86_64`: `arch/x86/entry/syscalls/syscall_64.tbl`.
pub const X86_64: Arch = Arch {
    name: "x86_64",
    table: &[
        (2, sys::OPEN),
        (42, sys::CONNECT),
        (59, sys::EXECVE),
        (76, sys::TRUNCATE),
        (80, sys::CHDIR),
        (81, sys::FCHDIR),
        (82, sys::RENAME),
        (83, sys::MKDIR),
        (84, sys::RMDIR),
        (85, sys::CREAT),
        (87, sys::UNLINK),
        (90, sys::CHMOD),
        (231, sys::EXIT_GROUP),
        (257, sys::OPENAT),
        (258, sys::MKDIRAT),
        (263, sys::UNLINKAT),
        (264, sys::RENAMEAT),
        (268, sys::FCHMODAT),
        (316, sys::RENAMEAT2),
        (322, sys::EXECVEAT),
        (437, sys::OPENAT2),
        (452, sys::FCHMODAT2),
    ],
    // di, si, dx, r10, r8, r9 in `struct pt_regs`.
    regs_args: [112, 104, 96, 56, 72, 64],
    // `task_struct` starts with `struct thread_info { flags; syscall_work;
    // u32 status; … }` (5.11+); `TS_COMPAT` marks an ia32 syscall, whose
    // numbers come from `syscall_32.tbl`.
    compat_offset: 16,
    compat_mask: 0x0002,
};

/// aarch64: the generic table (`include/uapi/asm-generic/unistd.h`), which
/// has no legacy `open`/`unlink`/`rename`/… calls.
pub const AARCH64: Arch = Arch {
    name: "aarch64",
    table: &[
        (34, sys::MKDIRAT),
        (35, sys::UNLINKAT),
        (38, sys::RENAMEAT),
        (45, sys::TRUNCATE),
        (49, sys::CHDIR),
        (50, sys::FCHDIR),
        (53, sys::FCHMODAT),
        (56, sys::OPENAT),
        (94, sys::EXIT_GROUP),
        (203, sys::CONNECT),
        (221, sys::EXECVE),
        (276, sys::RENAMEAT2),
        (281, sys::EXECVEAT),
        (437, sys::OPENAT2),
        (452, sys::FCHMODAT2),
    ],
    // regs[0..6]; regs[0] still holds the first argument at entry.
    regs_args: [0, 8, 16, 24, 32, 40],
    // AArch32 tasks are not told apart (documented limitation).
    compat_offset: u32::MAX,
    compat_mask: 0,
};

/// The architecture this binary runs on.
pub fn host() -> Option<Arch> {
    if cfg!(target_arch = "x86_64") {
        Some(X86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(AARCH64)
    } else {
        None
    }
}
