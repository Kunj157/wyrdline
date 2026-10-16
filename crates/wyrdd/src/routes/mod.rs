//! Route table.

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
        .fallback(fallback)
}

/// Unknown routes answer with a problem, too.
async fn fallback() -> Problem {
    Problem::not_found("no such endpoint")
}
