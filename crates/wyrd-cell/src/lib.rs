//! The Cell: a copy-on-write sandbox for one agent session.
//!
//! A Cell is a cgroup, a private mount namespace with an overlay mounted at
//! the workspace's own path (real workspace = read-only lower layer, one
//! upper layer per branch), and a Landlock ruleset that confines writes.
//! Checkpoints are CAS manifests of the upper layer; rewind and fork
//! materialise a checkpoint into a fresh branch. See ADR-005.

pub mod apply;
pub mod baseline;
pub mod cgroup;
pub mod confine;
pub mod diff;
pub mod exec;
pub mod launch;
pub mod paths;
pub mod session;
pub mod state;

pub use paths::{DataDir, SessionDir};
pub use session::{RunOptions, Session};

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CellError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("system call failed: {0}")]
    Sys(String),
    #[error(transparent)]
    Cas(#[from] wyrd_cas::CasError),
    #[error(transparent)]
    Ledger(#[from] wyrd_ledger::LedgerError),
    #[error("{0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(
        "Cell mode needs a mount namespace, but unprivileged user namespaces are blocked \
         ({0}). Install the AppArmor profile (`make apparmor-install`), run as root, or use \
         `wyrd run --observe` (no checkpoints or rewind)."
    )]
    NoNamespace(String),
    #[error("apply refused; the real workspace changed since the session started: {0:?}")]
    Conflicts(Vec<String>),
}

pub type Result<T> = std::result::Result<T, CellError>;

pub(crate) fn io_err(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> CellError + '_ {
    move |source| CellError::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub(crate) fn sys_err(what: &'static str) -> impl FnOnce(nix::Error) -> CellError {
    move |e| CellError::Sys(format!("{what}: {e}"))
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `CLOCK_MONOTONIC` in nanoseconds.
pub fn mono_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(ts.tv_sec).unwrap_or(0) * 1_000_000_000 + u64::try_from(ts.tv_nsec).unwrap_or(0)
}
