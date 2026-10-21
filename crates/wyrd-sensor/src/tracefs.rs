//! Tracepoint field offsets from tracefs `format` files.

use std::path::{Path, PathBuf};

use wyrd_common::Offsets;

use crate::{Result, SensorError};

/// Where tracefs is mounted (newer kernels, then the debugfs location).
pub fn root() -> Result<PathBuf> {
    for p in ["/sys/kernel/tracing", "/sys/kernel/debug/tracing"] {
        if Path::new(p).join("events").is_dir() {
            return Ok(PathBuf::from(p));
        }
    }
    Err(SensorError::Tracefs(
        "tracefs is not mounted or not readable (needs root)".into(),
    ))
}

/// Offset of `field` in a `format` file's text, if present.
pub fn field_offset(format: &str, field: &str) -> Option<u32> {
    format.lines().find_map(|l| {
        let l = l.trim();
        let decl = l.strip_prefix("field:")?.split(';').next()?.trim();
        let name = decl.rsplit([' ', '*']).next()?.split('[').next()?;
        if name != field {
            return None;
        }
        let off = l
            .split(';')
            .find_map(|p| p.trim().strip_prefix("offset:"))?;
        off.trim().parse().ok()
    })
}

/// Parse offsets from the two layout-dependent tracepoints (the
/// architecture fields are left neutral; the loader fills them).
pub fn offsets_from(newtask: &str, exit: &str) -> Result<Offsets> {
    let need = |fmt: &str, f: &str| {
        field_offset(fmt, f).ok_or_else(|| SensorError::Tracefs(format!("field {f} not found")))
    };
    Ok(Offsets {
        newtask_pid: need(newtask, "pid")?,
        newtask_clone_flags: need(newtask, "clone_flags")?,
        exit_pid: need(exit, "pid")?,
        exit_group_dead: field_offset(exit, "group_dead").unwrap_or(u32::MAX),
        regs_args: [0; 6],
        compat_offset: u32::MAX,
        compat_mask: 0,
    })
}

pub fn read_offsets(root: &Path) -> Result<Offsets> {
    let read = |rel: &str| {
        std::fs::read_to_string(root.join(rel))
            .map_err(|e| SensorError::Tracefs(format!("{rel}: {e}")))
    };
    offsets_from(
        &read("events/task/task_newtask/format")?,
        &read("events/sched/sched_process_exit/format")?,
    )
}
