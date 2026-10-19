//! Route table.

mod actions;
mod asks;
mod ingest;
mod policies;
mod sessions;
mod stream;
mod system;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};

pub use system::doctor_report;

use crate::AppState;
use crate::problem::Problem;

pub fn api() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/health", get(system::health))
        .route("/v1/ready", get(system::ready))
        .route("/v1/capabilities", get(system::capabilities))
        .route("/v1/metrics", get(system::metrics))
        .route("/v1/sessions", get(sessions::list))
        .route("/v1/sessions/{sid}", get(sessions::get))
        .route("/v1/sessions/{sid}/events", get(sessions::events))
        .route("/v1/sessions/{sid}/graph", get(sessions::graph))
        .route(
            "/v1/sessions/{sid}/tool-calls/{tcid}",
            get(sessions::tool_call),
        )
        .route("/v1/sessions/{sid}/stream", get(stream::stream))
        .route("/v1/sessions/{sid}/verify", get(sessions::verify))
        .route(
            "/v1/sessions/{sid}/checkpoints",
            get(sessions::checkpoints).post(actions::checkpoint),
        )
        .route("/v1/sessions/{sid}/stop", post(actions::stop))
        .route(
            "/v1/policies",
            get(policies::list)
                .post(policies::upload)
                .layer(DefaultBodyLimit::max(wyrd_policy::MAX_COMPONENT_BYTES)),
        )
        .route("/v1/policies/{pid}/backtest", post(policies::backtest))
        .route("/v1/sessions/{sid}/asks", get(asks::list))
        .route("/v1/sessions/{sid}/asks/{id}", post(asks::answer))
        .route(
            "/v1/sessions/{sid}/checkpoints/{target}/rewind",
            post(actions::rewind),
        )
        .route(
            "/v1/sessions/{sid}/checkpoints/{target}/fork",
            post(actions::fork),
        )
        .route(
            "/v1/sessions/{sid}/checkpoints/{target}/apply",
            post(actions::apply),
        )
        .route("/v1/sessions/{sid}/diff/{a}/{b}", get(actions::diff))
        .route(
            "/v1/sessions/{sid}/diff/{a}/{b}/file",
            get(actions::diff_file),
        )
        .fallback(fallback)
}

/// Unknown API routes answer with a problem; anything else is the Studio
/// (its single-page routes fall back to `index.html`) when one is served.
async fn fallback(State(st): State<Arc<AppState>>, req: Request) -> Response {
    match &st.studio {
        Some(dir) if !req.uri().path().starts_with("/v1/") && req.method() == Method::GET => {
            let index = dir.join("index.html");
            match ServeDir::new(dir)
                .fallback(ServeFile::new(index))
                .oneshot(req)
                .await
            {
                Ok(res) => res.into_response(),
                Err(e) => Problem::internal(e.to_string()).into_response(),
            }
        }
        _ => Problem::not_found("no such endpoint").into_response(),
    }
}

/// Ingest routes: authorised separately (per-session ingest token on TCP).
pub fn ingest() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/sessions/{sid}/otlp/v1/traces", post(ingest::traces))
        .route(
            "/v1/sessions/{sid}/ingest/hooks/{adapter}",
            post(ingest::hook),
        )
        .layer(DefaultBodyLimit::max(ingest::MAX_BODY))
}
