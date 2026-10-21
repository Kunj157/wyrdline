//! Wyrdline kernel programs.
//!
//! Every program first checks that the current task belongs to a registered
//! session cgroup (or a descendant of one); events from other tasks never
//! leave the kernel. Syscalls are seen through the raw `sys_enter` and
//! `sys_exit` tracepoints, which call the programs directly on every kernel;
//! the syscall table and `pt_regs` argument offsets of the host architecture,
//! and the field offsets of the two scheduler tracepoints whose layout
//! differs between kernels, are filled by userspace (`SYSCALLS`, `OFFSETS`).
#![cfg_attr(target_arch = "bpf", no_std)]
#![cfg_attr(target_arch = "bpf", no_main)]

#[cfg(target_arch = "bpf")]
mod prog;

#[cfg(target_arch = "bpf")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Host builds (`cargo build --workspace`) compile this stub; the real
/// object is built for `bpfel-unknown-none` by `wyrd-sensor`'s build script.
#[cfg(not(target_arch = "bpf"))]
fn main() {}

#[cfg(target_arch = "bpf")]
#[unsafe(no_mangle)]
#[unsafe(link_section = "license")]
pub static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
