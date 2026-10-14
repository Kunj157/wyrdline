//! cgroup v2 operations on a session's cgroup.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{CellError, Result, io_err};

pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// A cgroup directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cgroup {
    path: PathBuf,
}

/// Parse the unified-hierarchy line (`0::/path`) of `/proc/<pid>/cgroup`.
pub fn parse_proc_cgroup(text: &str) -> Option<PathBuf> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| Path::new(CGROUP_ROOT).join(p.trim().trim_start_matches('/')))
}

impl Cgroup {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Cgroup { path: path.into() }
    }

    pub fn of_pid(pid: u32) -> Result<Self> {
        let p = PathBuf::from(format!("/proc/{pid}/cgroup"));
        let text = fs::read_to_string(&p).map_err(io_err(&p))?;
        parse_proc_cgroup(&text)
            .map(Cgroup::at)
            .ok_or_else(|| CellError::Invalid(format!("no cgroup v2 entry for pid {pid}")))
    }

    /// Create `parent/name` (and `parent`).
    pub fn create(parent: &Path, name: &str) -> Result<Self> {
        let path = parent.join(name);
        fs::create_dir_all(&path).map_err(io_err(&path))?;
        Ok(Cgroup { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, file: &str, value: &str) -> Result<()> {
        let p = self.path.join(file);
        fs::write(&p, value).map_err(io_err(&p))
    }

    fn events(&self) -> Result<String> {
        let p = self.path.join("cgroup.events");
        fs::read_to_string(&p).map_err(io_err(&p))
    }

    fn event_flag(&self, key: &str) -> Result<bool> {
        Ok(self
            .events()?
            .lines()
            .any(|l| l.trim() == format!("{key} 1")))
    }

    pub fn exists(&self) -> bool {
        self.path.join("cgroup.procs").exists()
    }

    pub fn is_populated(&self) -> bool {
        self.event_flag("populated").unwrap_or(false)
    }

    pub fn procs(&self) -> Result<Vec<u32>> {
        let p = self.path.join("cgroup.procs");
        let text = fs::read_to_string(&p).map_err(io_err(&p))?;
        Ok(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
    }

    /// Move a process into this cgroup.
    pub fn add(&self, pid: u32) -> Result<()> {
        self.write("cgroup.procs", &pid.to_string())
    }

    fn wait_for(&self, timeout: Duration, mut cond: impl FnMut(&Self) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if cond(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond(self)
    }

    /// Freeze every process and wait until the kernel reports `frozen 1`.
    pub fn freeze(&self) -> Result<()> {
        self.write("cgroup.freeze", "1")?;
        if self.wait_for(Duration::from_secs(5), |c| {
            !c.is_populated() || c.event_flag("frozen").unwrap_or(false)
        }) {
            Ok(())
        } else {
            Err(CellError::Sys(format!(
                "{} did not freeze",
                self.path.display()
            )))
        }
    }

    pub fn thaw(&self) -> Result<()> {
        self.write("cgroup.freeze", "0")
    }

    /// Kill every process (`cgroup.kill`, falling back to SIGKILL per pid)
    /// and wait until the cgroup is empty.
    pub fn kill(&self) -> Result<()> {
        if self.write("cgroup.kill", "1").is_err() {
            for pid in self.procs()? {
                let pid = nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap_or(0));
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
            }
        }
        // Frozen tasks receive SIGKILL too, but thaw so they can exit promptly.
        let _ = self.thaw();
        self.wait_empty(Duration::from_secs(10))
    }

    pub fn wait_empty(&self, timeout: Duration) -> Result<()> {
        if self.wait_for(timeout, |c| !c.is_populated()) {
            Ok(())
        } else {
            Err(CellError::Sys(format!(
                "{} still has processes",
                self.path.display()
            )))
        }
    }

    /// Remove the (empty) cgroup directory.
    pub fn remove(&self) -> Result<()> {
        match fs::remove_dir(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(&self.path)(e)),
        }
    }
}
