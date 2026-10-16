//! Authentication: the bearer token for loopback TCP, and the peer-UID
//! check for the unix socket.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::problem::Problem;

/// `$XDG_STATE_HOME/wyrdline/token`, else `~/.local/state/wyrdline/token`.
pub fn default_token_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map_or_else(|| PathBuf::from("/"), PathBuf::from)
                .join(".local/state")
        },
        PathBuf::from,
    );
    base.join("wyrdline/token")
}

/// Read the token at `path`, creating it (0600, directory 0700) if absent.
pub fn load_or_create_token(path: &Path) -> std::io::Result<String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    match fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => return Ok(t.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    write_new_token(path)
}

/// Replace the token at `path` with a fresh one and return it.
pub fn write_new_token(path: &Path) -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token = hex::encode(bytes);
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(token)
}

/// Equality whose running time does not depend on where the inputs differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The ingest token of session `sid`: derived one-way from the daemon
/// token, so an agent can be given it without learning the daemon token.
pub fn ingest_token(daemon_token: &str, sid: &str) -> String {
    let key = blake3::hash(daemon_token.as_bytes());
    let mut h = blake3::Hasher::new_keyed(key.as_bytes());
    h.update(b"wyrdline ingest v1\0");
    h.update(sid.as_bytes());
    h.finalize().to_hex().to_string()
}

fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Middleware for ingest routes on TCP: the bearer must be the ingest
/// token of the session in the path (`/v1/sessions/{sid}/…`).
pub async fn require_ingest_token(
    State(token): State<Arc<str>>,
    req: Request,
    next: Next,
) -> Response {
    let sid = req
        .uri()
        .path()
        .strip_prefix("/v1/sessions/")
        .and_then(|r| r.split('/').next())
        .unwrap_or("");
    let ok = !sid.is_empty()
        && bearer(&req)
            .is_some_and(|t| constant_time_eq(t.as_bytes(), ingest_token(&token, sid).as_bytes()));
    if ok {
        next.run(req).await
    } else {
        Problem::unauthorized().into_response()
    }
}

/// Middleware for the TCP listener: require `Authorization: Bearer <token>`.
pub async fn require_token(State(token): State<Arc<str>>, req: Request, next: Next) -> Response {
    let ok = bearer(&req).is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        Problem::unauthorized().into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn ingest_tokens_are_per_session_and_per_daemon() {
        let a = ingest_token("daemon-1", "01aaa");
        assert_eq!(a.len(), 64);
        assert_eq!(a, ingest_token("daemon-1", "01aaa"));
        assert_ne!(a, ingest_token("daemon-1", "01aab"));
        assert_ne!(a, ingest_token("daemon-2", "01aaa"));
        assert!(!a.contains("daemon-1"));
    }

    #[test]
    fn token_file_is_private_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/wyrdline/token");
        let t1 = load_or_create_token(&path).unwrap();
        assert_eq!(t1.len(), 64);
        assert_eq!(load_or_create_token(&path).unwrap(), t1);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let t2 = write_new_token(&path).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(mode(&path), 0o600);
    }
}
