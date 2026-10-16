//! The intent socket server: a unix socket owned by the session's user,
//! one thread per connection, one request per connection.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::protocol::{ProtocolError, read_line};
use crate::{Ingest, Request, Response, Sink};

/// How long a client may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Unix socket paths are limited to 107 bytes.
pub const MAX_SOCKET_PATH: usize = 107;

/// Where the intent socket of a session lives: under `$XDG_RUNTIME_DIR`
/// when possible (short path, never writable by the Cell), else in the
/// session directory.
pub fn socket_path(session_dir: &Path, session_id: &str) -> PathBuf {
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(rt).join("wyrdline");
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
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("socket path too long: {}", path.display()),
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
