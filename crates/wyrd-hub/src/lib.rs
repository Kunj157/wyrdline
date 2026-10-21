//! Wyrdline Hub: the self-hosted team server (plan §4.4, §6.6, §14.4).
//! Bundles are stored in S3-compatible object storage, their metadata,
//! organisations, users, and the audit log in PostgreSQL.
//! `docs/architecture/hub.md` describes the API and the deployment.

pub mod audit;
pub mod auth;
pub mod config;
pub mod db;
pub mod jwt;
pub mod metrics;
pub mod oidc;
pub mod problem;
mod routes;
pub mod storage;

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::db::PgPool;
use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::Response;

pub use config::Config;
pub use problem::Problem;
pub use storage::Storage;

/// Header carrying the request id on every response.
pub static REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Everything the handlers need.
#[derive(Debug)]
pub struct AppState {
    pub config: Config,
    db: OnceLock<PgPool>,
    pub storage: Storage,
    /// `None` when no OIDC provider is configured (login disabled).
    pub oidc: Option<oidc::Provider>,
    pub metrics: metrics::Metrics,
}

impl AppState {
    /// A Hub whose database is not connected yet (see [`AppState::set_db`]).
    pub fn new(config: Config, storage: Storage) -> Self {
        let oidc = config.oidc.clone().map(oidc::Provider::new);
        AppState {
            config,
            db: OnceLock::new(),
            storage,
            oidc,
            metrics: metrics::Metrics::default(),
        }
    }

    /// The database is connected and migrated.
    pub fn set_db(&self, pool: PgPool) {
        let _ = self.db.set(pool);
    }

    /// The pool, or 503 while the Hub is still connecting.
    pub fn db(&self) -> Result<&PgPool, Problem> {
        self.db
            .get()
            .ok_or_else(|| Problem::unavailable("the database is not connected yet"))
    }

    fn gauges(&self) -> metrics::Gauges {
        self.db.get().map_or_else(metrics::Gauges::default, |p| {
            let idle = u64::try_from(p.num_idle()).unwrap_or(0);
            metrics::Gauges {
                ready: true,
                db_idle: idle,
                db_busy: u64::from(p.size()).saturating_sub(idle),
            }
        })
    }
}

/// The Hub's router.
pub fn app(state: Arc<AppState>) -> Router {
    routes::router()
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .layer(middleware::from_fn(secure_headers))
        .with_state(state)
}

/// Content-Security-Policy of every response (the API serves no pages).
pub const CSP: &str = "default-src 'none'; frame-ancestors 'none'; base-uri 'none'";

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
    h.entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    res
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
    // The route template, not the path: ids never become label values.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_string();
    let method = req.method().clone();
    let start = Instant::now();
    let mut res = next.run(req).await;
    let took = start.elapsed();
    let status = res.status().as_u16();
    let class = format!("{}xx", status / 100);
    state.metrics.inc(
        "wyrd_hub_http_requests_total",
        &[
            ("method", method.as_str()),
            ("route", &route),
            ("class", &class),
        ],
    );
    state.metrics.observe(
        "wyrd_hub_http_request_duration_seconds",
        &[("route", &route)],
        took,
    );
    tracing::info!(
        request_id = id.to_str().unwrap_or(""),
        %method,
        route,
        status,
        latency_ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        "request"
    );
    res.headers_mut().insert(REQUEST_ID.clone(), id);
    res
}

/// JSON logs on stderr, filtered by `WYRD_LOG` (default `info`).
pub fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("WYRD_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_current_span(false)
        .try_init();
}
