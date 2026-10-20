//! Liveness, readiness, and metrics.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
}

/// The process is up (no dependencies checked).
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Readiness {
    ready: bool,
    checks: Vec<Check>,
}

/// The database (connected, migrated, answering) and the bucket are
/// reachable: 200, else 503 naming what is not.
async fn readyz(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let database = match st.db() {
        Err(_) => Err("not connected yet".to_string()),
        Ok(pool) => {
            let ping = crate::db::query_scalar::<i32>("SELECT 1").fetch_one(pool);
            match tokio::time::timeout(Duration::from_secs(2), ping).await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "readiness: database");
                    Err("query failed".to_string())
                }
                Err(_) => Err("timed out".to_string()),
            }
        }
    };
    let storage = match tokio::time::timeout(Duration::from_secs(2), st.storage.ping()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "readiness: storage");
            Err("bucket unreachable".to_string())
        }
        Err(_) => Err("timed out".to_string()),
    };
    let checks = vec![
        Check {
            name: "database",
            ok: database.is_ok(),
            error: database.err(),
        },
        Check {
            name: "storage",
            ok: storage.is_ok(),
            error: storage.err(),
        },
    ];
    let ready = checks.iter().all(|c| c.ok);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(Readiness { ready, checks }))
}

async fn metrics(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        st.metrics.render(st.gauges()),
    )
}
