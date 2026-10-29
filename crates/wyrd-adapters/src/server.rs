//! The intent socket server: a unix socket owned by the session's user,
//! one thread per connection, one request per connection.

use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::protocol::{ProtocolError, read_line};
use crate::{Ingest, Request, Response, Sink};

/// How long a client may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Unix socket paths are limited to 107 bytes.
pub const MAX_SOCKET_PATH: usize = 107;

/// Where the intent socket of a session lives. The socket must sit in a
/// short path (see [`MAX_SOCKET_PATH`]), so the per-user runtime directory
/// comes first: `$XDG_RUNTIME_DIR`, else `/run/user/<uid>` when that is
/// the user's own directory. Both are private to the user and not writable
/// by the Cell. The session directory is the last resort.
pub fn socket_path(session_dir: &Path, session_id: &str) -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(default_runtime_dir);
    socket_path_in(session_dir, session_id, runtime.as_deref())
}

/// `/run/user/<uid>` when it exists and belongs to the effective user.
fn default_runtime_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(format!("/run/user/{}", rustix::process::geteuid().as_raw()));
    let meta = std::fs::metadata(&dir).ok()?;
    (meta.is_dir() && meta.uid() == rustix::process::geteuid().as_raw()).then_some(dir)
}

/// [`socket_path`] with the runtime directory given explicitly.
fn socket_path_in(session_dir: &Path, session_id: &str, runtime: Option<&Path>) -> PathBuf {
    if let Some(rt) = runtime {
        let dir = rt.join("wyrdline");
        let ok = std::fs::create_dir_all(&dir).is_ok()
            && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_ok();
        if ok {
            return dir.join(format!("intent-{session_id}.sock"));
        }
    }
    session_dir.join("intent.sock")
}

/// Bind the socket at `path` (replacing a stale one) with mode 0600.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "socket path is {len} bytes, over the {MAX_SOCKET_PATH}-byte limit: {}; \
                 set XDG_RUNTIME_DIR to a short directory or use a shorter WYRD_DATA_DIR",
                path.display()
            ),
        ));
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Connections being served right now, in this process.
static ACTIVE: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// Counts one connection from accept until its answer is written (or the
/// connection is refused), so [`drain`] can wait for it.
struct Active;

impl Active {
    fn enter() -> Self {
        *ACTIVE.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Active
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let mut n = ACTIVE.0.lock().unwrap_or_else(PoisonError::into_inner);
        *n -= 1;
        ACTIVE.1.notify_all();
    }
}

/// Wait, up to `deadline`, until no connection is being served. Call it
/// after the socket file is removed and before the process exits, so an
/// answer that is already being written reaches its client. Returns whether
/// the server went quiet in time.
pub fn drain(deadline: Duration) -> bool {
    let end = Instant::now() + deadline;
    let mut n = ACTIVE.0.lock().unwrap_or_else(PoisonError::into_inner);
    while *n > 0 {
        let now = Instant::now();
        if now >= end {
            return false;
        }
        n = ACTIVE
            .1
            .wait_timeout(n, end - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    true
}

/// Accept connections until the listener fails. Each connection is served
/// on its own thread.
pub fn serve<S: Sink + 'static>(listener: &UnixListener, ingest: &Arc<Ingest<S>>) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let ingest = Arc::clone(ingest);
                let spawned = std::thread::Builder::new()
                    .name("wyrd-intent-conn".into())
                    .spawn(move || serve_one(&stream, &ingest));
                if let Err(e) = spawned {
                    tracing::error!(error = %e, "intent socket: cannot spawn a handler");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "intent socket: accept failed");
                return;
            }
        }
    }
}

fn respond(mut stream: &UnixStream, resp: &Response) {
    let line = serde_json::to_vec(resp).map(|mut v| {
        v.push(b'\n');
        v
    });
    if let Err(e) = line
        .map_err(std::io::Error::from)
        .and_then(|l| stream.write_all(&l))
    {
        tracing::warn!(error = %e, "intent socket: could not send the response");
    }
}

/// Serve one connection: check the peer, read the request, answer.
pub fn serve_one<S: Sink>(stream: &UnixStream, ingest: &Ingest<S>) {
    let _active = Active::enter();
    // Defence in depth on top of the 0600 socket mode: only our own user.
    let me = rustix::process::getuid();
    match rustix::net::sockopt::socket_peercred(stream) {
        Ok(cred) if cred.uid == me => {}
        Ok(cred) => {
            tracing::warn!(
                uid = cred.uid.as_raw(),
                "intent socket: rejected another user"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "intent socket: no peer credentials");
            return;
        }
    }
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let resp = match read_line(stream) {
        Ok(Some(line)) => match serde_json::from_str::<Request>(&line) {
            Ok(req) => ingest.handle(req),
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        },
        Ok(None) => return,
        Err(ProtocolError::TooLong) => Response::Error {
            message: "request too long".into(),
        },
        Err(e) => {
            tracing::warn!(error = %e, "intent socket: read failed");
            return;
        }
    };
    respond(stream, &resp);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data directory deep enough that `intent.sock` will not fit.
    fn long_session_dir() -> PathBuf {
        Path::new("/tmp").join("d".repeat(120)).join("session")
    }

    #[test]
    fn a_long_session_dir_uses_the_runtime_dir_when_there_is_one() {
        let runtime = tempfile::tempdir().unwrap();
        let path = socket_path_in(&long_session_dir(), "01SESSION", Some(runtime.path()));
        assert_eq!(path, runtime.path().join("wyrdline/intent-01SESSION.sock"));
        let listener = bind(&path).expect("the runtime path fits the limit");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
    }

    #[test]
    fn a_long_path_with_no_runtime_dir_fails_naming_the_limit_and_the_fix() {
        let path = socket_path_in(&long_session_dir(), "01SESSION", None);
        assert!(path.as_os_str().len() > MAX_SOCKET_PATH);
        let err = bind(&path).expect_err("the path is over the limit");
        let text = err.to_string();
        assert!(text.contains("107-byte limit"), "{text}");
        assert!(text.contains("XDG_RUNTIME_DIR"), "{text}");
        assert!(!path.exists(), "nothing is created for a refused path");
    }
}
