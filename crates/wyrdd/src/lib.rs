//! `wyrdd`: the per-user Wyrdline daemon. Serves the HTTP API (plan §14.1)
//! over the session data directory, on a unix socket (same UID only) and on
//! loopback TCP (bearer token).

pub mod api;
pub mod auth;
pub mod idempotency;
pub mod listen;
pub mod openapi;
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
pub const DEFAULT_LISTEN: &str = wyrd_adapters::daemon::DEFAULT_LISTEN;

/// Everything the handlers need.
#[derive(Debug)]
pub struct AppState {
    pub data: DataDir,
    /// The `wyrd` binary, for probes that must run in a child process.
    pub wyrd_bin: PathBuf,
    pub metrics: Metrics,
    pub idempotency: idempotency::Store,
    /// Built Studio (`index.html` and assets), served on loopback TCP.
    pub studio: Option<PathBuf>,
}

impl AppState {
    pub fn new(data: DataDir, wyrd_bin: PathBuf) -> Self {
        AppState {
            data,
            wyrd_bin,
            metrics: Metrics::default(),
            idempotency: idempotency::Store::default(),
            studio: None,
        }
    }

    /// Serve the Studio build in `dir` (when it holds an `index.html`).
    #[must_use]
    pub fn with_studio(mut self, dir: Option<PathBuf>) -> Self {
        self.studio = dir.filter(|d| d.join("index.html").is_file());
        self
    }
}

/// Content-Security-Policy of every response: the Studio loads only its
/// own scripts (plus WebAssembly for the correlator) and talks only to
/// this origin.
pub const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; \
frame-ancestors 'none'; form-action 'none'";

/// Security headers for every response.
async fn secure_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    res
}

/// Counters exported on `/v1/metrics`.
#[derive(Debug, Default)]
pub struct Metrics {
    by_class: [AtomicU64; 5],
    /// Open SSE streams.
    pub streams: AtomicU64,
    /// Ingest requests by kind (OTLP, hook) and outcome.
    pub ingest: [[AtomicU64; 2]; 2],
    /// Policy backtests run.
    pub backtests: AtomicU64,
    /// Divergence triage updates written.
    pub triages: AtomicU64,
}

/// Ingest kinds, indexes into [`Metrics::ingest`].
pub const INGEST_OTLP: usize = 0;
pub const INGEST_HOOK: usize = 1;

/// Point-in-time numbers read from the data directory for a scrape.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Sessions by status name.
    pub sessions: std::collections::BTreeMap<String, u64>,
    /// Bytes of all session ledgers (including write-ahead logs).
    pub ledger_bytes: u64,
}

impl Snapshot {
    /// Read the data directory (blocking).
    pub fn read(data: &DataDir) -> Snapshot {
        let mut snap = Snapshot::default();
        for r in wyrd_cell::Session::list(data).unwrap_or_default() {
            let status = serde_json::to_value(r.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            *snap.sessions.entry(status).or_default() += 1;
            let ledger = data.session(&r.id).ledger();
            for suffix in ["", "-wal"] {
                let mut p = ledger.clone().into_os_string();
                p.push(suffix);
                snap.ledger_bytes += std::fs::metadata(p).map_or(0, |m| m.len());
            }
        }
        snap
    }
}

/// The `# HELP` and `# TYPE` lines of a metric family.
fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

impl Metrics {
    fn record(&self, status: u16) {
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        self.by_class[class].fetch_add(1, Ordering::Relaxed);
    }

    /// Count one ingest request of `kind` ([`INGEST_OTLP`], [`INGEST_HOOK`]).
    pub fn ingested(&self, kind: usize, ok: bool) {
        self.ingest[kind][usize::from(!ok)].fetch_add(1, Ordering::Relaxed);
    }

    pub fn backtest(&self) {
        self.backtests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn triage(&self) {
        self.triages.fetch_add(1, Ordering::Relaxed);
    }

    /// Prometheus text exposition.
    pub fn render(&self, snap: &Snapshot) -> String {
        let mut out = String::new();
        family(
            &mut out,
            "wyrdd_http_requests_total",
            "counter",
            "HTTP requests by status class.",
        );
        for (i, c) in self.by_class.iter().enumerate() {
            let _ = writeln!(
                out,
                "wyrdd_http_requests_total{{class=\"{}xx\"}} {}",
                i + 1,
                c.load(Ordering::Relaxed)
            );
        }
        family(
            &mut out,
            "wyrdd_ingest_requests_total",
            "counter",
            "Ingest requests by kind and outcome.",
        );
        for (k, kind) in [(INGEST_OTLP, "otlp"), (INGEST_HOOK, "hook")] {
            for (o, outcome) in [(0, "accepted"), (1, "rejected")] {
                let _ = writeln!(
                    out,
                    "wyrdd_ingest_requests_total{{kind=\"{kind}\",outcome=\"{outcome}\"}} {}",
                    self.ingest[k][o].load(Ordering::Relaxed)
                );
            }
        }
        family(
            &mut out,
            "wyrdd_policy_backtests_total",
            "counter",
            "Policy backtests run.",
        );
        let _ = writeln!(
            out,
            "wyrdd_policy_backtests_total {}",
            self.backtests.load(Ordering::Relaxed)
        );
        family(
            &mut out,
            "wyrdd_divergence_triages_total",
            "counter",
            "Divergence triage updates written to ledgers.",
        );
        let _ = writeln!(
            out,
            "wyrdd_divergence_triages_total {}",
            self.triages.load(Ordering::Relaxed)
        );
        family(
            &mut out,
            "wyrdd_sse_streams",
            "gauge",
            "Open session event streams.",
        );
        let _ = writeln!(
            out,
            "wyrdd_sse_streams {}",
            self.streams.load(Ordering::Relaxed)
        );
        family(
            &mut out,
            "wyrdd_sessions",
            "gauge",
            "Sessions in the data directory by status.",
        );
        for (status, n) in &snap.sessions {
            let _ = writeln!(out, "wyrdd_sessions{{status=\"{status}\"}} {n}");
        }
        family(
            &mut out,
            "wyrdd_ledger_bytes",
            "gauge",
            "Bytes of all session ledgers on disk.",
        );
        let _ = writeln!(out, "wyrdd_ledger_bytes {}", snap.ledger_bytes);
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
pub fn tcp_app(state: Arc<AppState>, tokens: Arc<auth::TokenSource>, cors: CorsLayer) -> Router {
    let token = tokens;
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
        .layer(middleware::from_fn(secure_headers))
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
        m.ingested(INGEST_OTLP, true);
        m.ingested(INGEST_HOOK, false);
        let mut snap = Snapshot::default();
        snap.sessions.insert("closed".into(), 2);
        snap.ledger_bytes = 4096;
        let text = m.render(&snap);
        assert!(text.contains("wyrdd_ingest_requests_total{kind=\"otlp\",outcome=\"accepted\"} 1"));
        assert!(text.contains("wyrdd_ingest_requests_total{kind=\"hook\",outcome=\"rejected\"} 1"));
        assert!(text.contains("wyrdd_sessions{status=\"closed\"} 2"));
        assert!(text.contains("wyrdd_ledger_bytes 4096"));
        assert!(text.contains("wyrdd_sse_streams 0"));
        assert!(text.contains("class=\"2xx\"} 2"));
        assert!(text.contains("class=\"4xx\"} 1"));
        assert!(text.contains("class=\"5xx\"} 0"));
    }
}
