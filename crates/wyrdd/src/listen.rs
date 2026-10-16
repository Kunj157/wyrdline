//! Listeners: a unix socket that only admits the daemon's own UID.

use std::io;
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

/// A unix-socket listener that drops connections from other users before
/// a single byte is read (`SO_PEERCRED`).
#[derive(Debug)]
pub struct SameUidListener {
    inner: UnixListener,
    uid: u32,
    path: PathBuf,
}

impl SameUidListener {
    /// Bind `path` (0600 inside a 0700 directory). A stale socket is
    /// replaced; a socket a live daemon still answers on is an error.
    pub fn bind(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a daemon is already listening on {}", path.display()),
                ));
            }
            std::fs::remove_file(path)?;
        }
        let inner = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(SameUidListener {
            inner,
            uid: rustix::process::getuid().as_raw(),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Whether a peer with `peer_uid` may talk to a daemon running as `uid`.
pub fn admit(peer_uid: Option<u32>, uid: u32) -> bool {
    peer_uid == Some(uid)
}

impl axum::serve::Listener for SameUidListener {
    type Io = UnixStream;
    type Addr = tokio::net::unix::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.inner.accept().await {
                Ok((stream, addr)) => {
                    let peer = stream.peer_cred().ok().map(|c| c.uid());
                    if admit(peer, self.uid) {
                        return (stream, addr);
                    }
                    tracing::warn!(peer_uid = ?peer, "refused unix-socket peer of another user");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

impl Drop for SameUidListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_uid_is_admitted() {
        assert!(admit(Some(1000), 1000));
        assert!(!admit(Some(0), 1000));
        assert!(!admit(Some(1001), 1000));
        assert!(!admit(None, 1000));
    }
}
