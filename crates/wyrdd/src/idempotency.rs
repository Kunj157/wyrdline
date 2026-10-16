//! `Idempotency-Key` for mutating requests: the first response for a key
//! is stored and replayed for retries of the same request.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::AppState;
use crate::problem::Problem;

/// How long a stored response is replayed.
pub const TTL: Duration = Duration::from_secs(24 * 3600);
/// Most keys kept; the oldest are evicted first.
pub const MAX_KEYS: usize = 1_000;
/// Largest request or response body buffered for replay.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
enum Slot {
    InFlight,
    Done {
        status: StatusCode,
        content_type: Option<HeaderValue>,
        body: Bytes,
    },
}

#[derive(Debug, Clone)]
struct Entry {
    fingerprint: u64,
    at: Instant,
    slot: Slot,
}

/// Stored responses by key.
#[derive(Debug, Default)]
pub struct Store {
    entries: Mutex<HashMap<String, Entry>>,
}

/// What to do with a request carrying a key.
#[derive(Debug)]
enum Claim {
    Run,
    Replay(Response),
    Reject(Problem),
}

impl Store {
    fn claim(&self, key: &str, fingerprint: u64) -> Claim {
        let Ok(mut map) = self.entries.lock() else {
            return Claim::Reject(Problem::internal("idempotency store poisoned"));
        };
        let now = Instant::now();
        map.retain(|_, e| now.duration_since(e.at) < TTL);
        if let Some(e) = map.get(key) {
            if e.fingerprint != fingerprint {
                return Claim::Reject(Problem::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "idempotency-key-reused",
                    "Idempotency-Key was used for a different request",
                ));
            }
            return match &e.slot {
                Slot::InFlight => Claim::Reject(Problem::new(
                    StatusCode::CONFLICT,
                    "idempotency-in-flight",
                    "A request with this Idempotency-Key is still running",
                )),
                Slot::Done {
                    status,
                    content_type,
                    body,
                } => {
                    let mut res = (*status, body.clone()).into_response();
                    if let Some(ct) = content_type {
                        res.headers_mut().insert(header::CONTENT_TYPE, ct.clone());
                    }
                    res.headers_mut()
                        .insert("idempotent-replayed", HeaderValue::from_static("true"));
                    Claim::Replay(res)
                }
            };
        }
        if map.len() >= MAX_KEYS
            && let Some(oldest) = map.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(
            key.to_string(),
            Entry {
                fingerprint,
                at: now,
                slot: Slot::InFlight,
            },
        );
        Claim::Run
    }

    fn finish(&self, key: &str, slot: Option<Slot>) {
        if let Ok(mut map) = self.entries.lock() {
            match slot {
                Some(slot) => {
                    if let Some(e) = map.get_mut(key) {
                        e.slot = slot;
                    }
                }
                // Not stored (e.g. a server error): allow a retry.
                None => {
                    map.remove(key);
                }
            }
        }
    }
}

fn fingerprint(method: &Method, path: &str, body: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    method.as_str().hash(&mut h);
    path.hash(&mut h);
    body.hash(&mut h);
    h.finish()
}

fn valid_key(v: &HeaderValue) -> Option<String> {
    let b = v.as_bytes();
    (!b.is_empty() && b.len() <= 128 && b.iter().all(u8::is_ascii_graphic))
        .then(|| String::from_utf8_lossy(b).into_owned())
}

/// Middleware: applies to POST and PATCH requests that carry the header.
pub async fn idempotent(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::POST | Method::PATCH) {
        return next.run(req).await;
    }
    let Some(raw) = req.headers().get("idempotency-key").cloned() else {
        return next.run(req).await;
    };
    let Some(key) = valid_key(&raw) else {
        return Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid-idempotency-key",
            "Idempotency-Key must be 1–128 visible ASCII characters",
        )
        .into_response();
    };
    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        return Problem::new(StatusCode::PAYLOAD_TOO_LARGE, "too-large", "Body too large")
            .into_response();
    };
    let fp = fingerprint(&parts.method, parts.uri.path(), &bytes);
    match st.idempotency.claim(&key, fp) {
        Claim::Replay(res) => return res,
        Claim::Reject(p) => return p.into_response(),
        Claim::Run => {}
    }
    let res = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    let (parts, body) = res.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        st.idempotency.finish(&key, None);
        return Problem::internal("response too large to store").into_response();
    };
    let slot = (!parts.status.is_server_error()).then(|| Slot::Done {
        status: parts.status,
        content_type: parts.headers.get(header::CONTENT_TYPE).cloned(),
        body: body.clone(),
    });
    st.idempotency.finish(&key, slot);
    Response::from_parts(parts, Body::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_same_request_replays_and_different_request_is_rejected() {
        let s = Store::default();
        assert!(matches!(s.claim("k", 1), Claim::Run));
        assert!(matches!(s.claim("k", 1), Claim::Reject(_)), "in flight");
        s.finish(
            "k",
            Some(Slot::Done {
                status: StatusCode::OK,
                content_type: None,
                body: Bytes::from_static(b"x"),
            }),
        );
        assert!(matches!(s.claim("k", 1), Claim::Replay(_)));
        match s.claim("k", 2) {
            Claim::Reject(p) => assert_eq!(p.status, 422),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn server_errors_are_not_stored_and_keys_are_bounded() {
        let s = Store::default();
        assert!(matches!(s.claim("k", 1), Claim::Run));
        s.finish("k", None);
        assert!(matches!(s.claim("k", 1), Claim::Run), "retry allowed");
        for i in 0..MAX_KEYS + 10 {
            let _ = s.claim(&format!("x{i}"), 0);
        }
        assert!(s.entries.lock().unwrap().len() <= MAX_KEYS);
    }

    #[test]
    fn keys_are_bounded_visible_ascii() {
        assert!(valid_key(&HeaderValue::from_static("abc-1")).is_some());
        assert!(valid_key(&HeaderValue::from_static("a b")).is_none());
        assert!(valid_key(&HeaderValue::from_str(&"k".repeat(129)).unwrap()).is_none());
    }
}
