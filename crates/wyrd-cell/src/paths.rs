//! On-disk layout of the Wyrdline data directory.

use std::path::{Path, PathBuf};

/// `$WYRD_DATA_DIR`, else `$XDG_DATA_HOME/wyrdline`, else `~/.local/share/wyrdline`.
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DataDir { root: root.into() }
    }

    pub fn from_env() -> Self {
        if let Some(d) = std::env::var_os("WYRD_DATA_DIR") {
            return DataDir::new(d);
        }
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            return DataDir::new(PathBuf::from(d).join("wyrdline"));
        }
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        DataDir::new(home.join(".local/share/wyrdline"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Uploaded policy components (`<id>.wasm`).
    pub fn policies(&self) -> PathBuf {
        self.root.join("policies")
    }

    pub fn cas(&self) -> PathBuf {
        self.root.join("cas")
    }

    pub fn host_key(&self) -> PathBuf {
        self.root.join("keys/host.key")
    }

    pub fn session(&self, id: &str) -> SessionDir {
        SessionDir {
            root: self.sessions().join(id),
        }
    }
}

/// `$XDG_STATE_HOME/wyrdline`, else `~/.local/state/wyrdline` (the daemon
/// token lives here).
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(d).join("wyrdline"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/wyrdline"))
}

/// Files of one session.
#[derive(Debug, Clone)]
pub struct SessionDir {
    root: PathBuf,
}

impl SessionDir {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ledger(&self) -> PathBuf {
        self.root.join("ledger.wyrdledger")
    }

    pub fn state(&self) -> PathBuf {
        self.root.join("session.json")
    }

    pub fn lock(&self) -> PathBuf {
        self.root.join("session.lock")
    }

    pub fn baseline(&self) -> PathBuf {
        self.root.join("baseline.cbor")
    }

    pub fn spec(&self) -> PathBuf {
        self.root.join("launch.json")
    }

    pub fn branch(&self, id: &str) -> PathBuf {
        self.root.join("branches").join(id)
    }

    pub fn upper(&self, branch: &str) -> PathBuf {
        self.branch(branch).join("upper")
    }

    pub fn work(&self, branch: &str) -> PathBuf {
        self.branch(branch).join("work")
    }
}
