//! HTTP routes.

mod analytics;
mod auth;
pub(crate) mod bundles;
mod health;
mod orgs;
mod sync;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};

use crate::Problem;

use crate::AppState;

/// Content-Security-Policy of the served Studio: its own scripts (and
/// WebAssembly for the correlator), talking only to this origin.
pub const STUDIO_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; \
frame-ancestors 'none'; form-action 'self'";

/// Unknown API paths are a 404 problem; with a Studio build configured,
/// any other `GET` is the Studio (its routes fall back to `index.html`).
async fn fallback(State(st): State<Arc<AppState>>, req: Request) -> Response {
    let studio = st
        .config
        .studio_dir
        .as_ref()
        .filter(|d| d.join("index.html").is_file());
    match studio {
        Some(dir) if !req.uri().path().starts_with("/api/") && req.method() == Method::GET => {
            let index = dir.join("index.html");
            match ServeDir::new(dir)
                .fallback(ServeFile::new(index))
                .oneshot(req)
                .await
            {
                Ok(res) => {
                    let mut res = res.into_response();
                    res.headers_mut().insert(
                        header::CONTENT_SECURITY_POLICY,
                        HeaderValue::from_static(STUDIO_CSP),
                    );
                    res
                }
                Err(e) => Problem::internal(&e).into_response(),
            }
        }
        _ => Problem::not_found().into_response(),
    }
}

pub fn router(max_upload_bytes: u64) -> Router<Arc<AppState>> {
    health::router()
        .merge(bundles::router(max_upload_bytes))
        .merge(analytics::router())
        .merge(auth::router())
        .merge(orgs::router())
        .merge(sync::router())
        .fallback(fallback)
}
