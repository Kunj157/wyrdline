//! Authentication: the bearer token for loopback TCP, and the peer-UID
//! check for the unix socket.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::problem::Problem;

pub use wyrd_adapters::daemon::{
    default_token_path, ingest_token, load_or_create_token, write_new_token,
};

/// The daemon token, re-read from its file when the file changes, so
/// `wyrd token rotate` takes effect without restarting the daemon.
#[derive(Debug)]
pub struct TokenSource {
    path: Option<PathBuf>,
    cached: Mutex<(Option<SystemTime>, Arc<str>)>,
}

impl TokenSource {
    /// A token that never changes (tests, embedding).
    pub fn fixed(token: &str) -> Self {
        TokenSource {
            path: None,
            cached: Mutex::new((None, Arc::from(token))),
        }
    }

    /// The token in `path` (created if absent).
    pub fn file(path: PathBuf) -> std::io::Result<Self> {
        let token = load_or_create_token(&path)?;
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        Ok(TokenSource {
            path: Some(path),
            cached: Mutex::new((mtime, Arc::from(token.as_str()))),
        })
    }

    /// The current token. A file that vanished or became empty keeps the
    /// last good token rather than opening the API.
    pub fn current(&self) -> Arc<str> {
        let mut c = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(path) = &self.path {
            let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            if mtime.is_some()
                && mtime != c.0
                && let Ok(t) = fs::read_to_string(path)
                && !t.trim().is_empty()
            {
                *c = (mtime, Arc::from(t.trim()));
            }
        }
        c.1.clone()
    }
}

/// Equality whose running time does not depend on where the inputs differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|t| !t.is_empty())
}

/// Middleware for ingest routes on TCP: the bearer must be the ingest
/// token of the session in the path (`/v1/sessions/{sid}/…`).
pub async fn require_ingest_token(
    State(tokens): State<Arc<TokenSource>>,
    req: Request,
    next: Next,
) -> Response {
    let sid = req
        .uri()
        .path()
        .strip_prefix("/v1/sessions/")
        .and_then(|r| r.split('/').next())
        .unwrap_or("");
    let token = tokens.current();
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
pub async fn require_token(
    State(tokens): State<Arc<TokenSource>>,
    req: Request,
    next: Next,
) -> Response {
    let token = tokens.current();
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
    fn rotated_tokens_are_picked_up_and_empty_files_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wyrdline/token");
        let src = TokenSource::file(path.clone()).unwrap();
        let t1 = src.current();
        assert_eq!(t1.len(), 64);
        // Ensure a different mtime even on coarse-timestamp filesystems.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let t2 = write_new_token(&path).unwrap();
        assert_eq!(&*src.current(), t2.as_str());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "").unwrap();
        assert_eq!(
            &*src.current(),
            t2.as_str(),
            "an emptied file keeps the last token"
        );
        assert_eq!(&*TokenSource::fixed("x").current(), "x");
    }
}
