//! Wyrdline kernel programs.
//!
//! Every program first checks that the current task belongs to a registered
//! session cgroup (or a descendant of one); events from other tasks never
//! leave the kernel. Syscall tracepoints are used because their argument
//! layout (`args[i]` at offset 16 + 8·i) is stable across kernels; the two
//! scheduler tracepoints whose layout differs between kernels read their
//! field offsets from `OFFSETS`, filled by userspace from tracefs.
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
