//! Route table.

mod sessions;
mod system;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

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
        .route("/v1/sessions/{sid}/verify", get(sessions::verify))
        .route("/v1/sessions/{sid}/checkpoints", get(sessions::checkpoints))
        .fallback(fallback)
}

/// Unknown routes answer with a problem, too.
async fn fallback() -> Problem {
    Problem::not_found("no such endpoint")
}
