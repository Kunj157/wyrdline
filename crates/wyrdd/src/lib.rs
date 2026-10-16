//! `wyrdd`: the per-user Wyrdline daemon. Serves the HTTP API (plan §14.1)
//! over the session data directory, on a unix socket (same UID only) and on
//! loopback TCP (bearer token).

pub mod auth;
pub mod idempotency;
pub mod listen;
pub mod problem;
mod routes;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use tower_http::cors::{AllowOrigin, CorsLayer};
use wyrd_cell::DataDir;

pub use routes::doctor_report;

/// Header carrying the request id on every response.
pub static REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Default loopback address (plan §11.1).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7717";

/// Everything the handlers need.
#[derive(Debug)]
pub struct AppState {
    pub data: DataDir,
    /// The `wyrd` binary, for probes that must run in a child process.
    pub wyrd_bin: PathBuf,
    pub metrics: Metrics,
    pub idempotency: idempotency::Store,
}

impl AppState {
    pub fn new(data: DataDir, wyrd_bin: PathBuf) -> Self {
        AppState {
            data,
            wyrd_bin,
            metrics: Metrics::default(),
            idempotency: idempotency::Store::default(),
        }
    }
}

/// Request counters exported on `/v1/metrics`.
#[derive(Debug, Default)]
pub struct Metrics {
    by_class: [AtomicU64; 5],
}

impl Metrics {
    fn record(&self, status: u16) {
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        self.by_class[class].fetch_add(1, Ordering::Relaxed);
    }

    /// Prometheus text exposition.
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# HELP wyrdd_http_requests_total HTTP requests by status class.\n\
             # TYPE wyrdd_http_requests_total counter\n",
        );
        for (i, c) in self.by_class.iter().enumerate() {
            let _ = writeln!(
                out,
                "wyrdd_http_requests_total{{class=\"{}xx\"}} {}",
                i + 1,
                c.load(Ordering::Relaxed)
            );
        }
        out
    }
}

/// A client-supplied request id is kept if it is short, visible ASCII.
fn valid_request_id(v: &HeaderValue) -> bool {
    let b = v.as_bytes();
    !b.is_empty() && b.len() <= 128 && b.iter().all(u8::is_ascii_graphic)
}

/// Request id, access log, and metrics for every request.
async fn observe(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&REQUEST_ID)
        .filter(|v| valid_request_id(v))
        .cloned()
        .unwrap_or_else(|| {
            HeaderValue::from_str(&ulid::Ulid::generate().to_string())
                .unwrap_or(HeaderValue::from_static("unknown"))
        });
    req.headers_mut().insert(REQUEST_ID.clone(), id.clone());
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let start = Instant::now();
    let mut res = next.run(req).await;
    let status = res.status().as_u16();
    state.metrics.record(status);
    tracing::info!(
        request_id = id.to_str().unwrap_or(""),
        %method,
        path,
        status,
        latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "request"
    );
    res.headers_mut().insert(REQUEST_ID.clone(), id);
    res
}

/// Origins allowed to call the TCP API from a browser: the daemon's own
/// (Studio is served by `wyrdd`) and, for development, `extra`.
pub fn cors(listen: &str, extra: Option<&str>) -> CorsLayer {
    let port = listen.rsplit(':').next().unwrap_or("7717");
    let mut origins: Vec<HeaderValue> = [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ]
    .iter()
    .filter_map(|o| HeaderValue::from_str(o).ok())
    .collect();
    origins.extend(extra.and_then(|o| HeaderValue::from_str(o).ok()));
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            REQUEST_ID.clone(),
            HeaderName::from_static("idempotency-key"),
            HeaderName::from_static("last-event-id"),
        ])
        .expose_headers([REQUEST_ID.clone()])
}

/// The API as served on the unix socket (the peer UID is checked by the
/// listener, so no token is needed).
pub fn unix_app(state: Arc<AppState>) -> Router {
    routes::api()
        .merge(routes::ingest())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            idempotency::idempotent,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .with_state(state)
}

/// The API as served on loopback TCP: bearer token and CORS.
pub fn tcp_app(state: Arc<AppState>, token: &str, cors: CorsLayer) -> Router {
    let token: Arc<str> = Arc::from(token);
    // The last route layer runs first: authentication happens before an
    // idempotency key is ever claimed (a test pins this).
    routes::api()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            idempotency::idempotent,
        ))
        .route_layer(middleware::from_fn_with_state(
            token.clone(),
            auth::require_token,
        ))
        .merge(routes::ingest().route_layer(middleware::from_fn_with_state(
            token,
            auth::require_ingest_token,
        )))
        .layer(cors)
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_from_clients_are_bounded() {
        assert!(valid_request_id(&HeaderValue::from_static("abc-123")));
        assert!(!valid_request_id(&HeaderValue::from_static("")));
        assert!(!valid_request_id(&HeaderValue::from_static("a b")));
        let long = "x".repeat(129);
        assert!(!valid_request_id(&HeaderValue::from_str(&long).unwrap()));
    }

    #[test]
    fn metrics_count_by_class() {
        let m = Metrics::default();
        m.record(200);
        m.record(204);
        m.record(401);
        let text = m.render();
        assert!(text.contains("class=\"2xx\"} 2"));
        assert!(text.contains("class=\"4xx\"} 1"));
        assert!(text.contains("class=\"5xx\"} 0"));
    }
}
