//! Liveness, readiness, capabilities, metrics.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;

use crate::AppState;
use crate::api::{Health, Ready};
use crate::problem::Problem;

pub async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

/// Ready when the data directory exists (or can be created) and is writable.
pub async fn ready(State(s): State<Arc<AppState>>) -> Result<Json<Ready>, Problem> {
    let root = s.data.root().to_path_buf();
    let ok = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&root).is_ok() && tempfile_probe(&root)
    })
    .await
    .unwrap_or(false);
    if ok {
        Ok(Json(Ready {
            status: "ready".into(),
        }))
    } else {
        Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not-ready",
            "Data directory is not writable",
        ))
    }
}

fn tempfile_probe(root: &Path) -> bool {
    let p = root.join(format!(".wyrdd-ready-{}", std::process::id()));
    let ok = std::fs::write(&p, b"").is_ok();
    let _ = std::fs::remove_file(&p);
    ok
}

/// Doctor report for this machine. The Landlock ABI is probed through
/// `wyrd __probe-landlock` in a child, since restricting is irreversible.
pub fn doctor_report(wyrd_bin: &Path) -> wyrd_doctor::Report {
    let landlock_abi = Command::new(wyrd_bin)
        .arg("__probe-landlock")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok());
    let mut env = wyrd_doctor::Env::new(
        "/",
        wyrd_doctor::Probes {
            landlock_abi,
            uid: rustix::process::getuid().as_raw(),
        },
    );
    env.mask = std::env::var("WYRD_DOCTOR_MASK").ok();
    wyrd_doctor::run(&env)
}

pub async fn capabilities(
    State(s): State<Arc<AppState>>,
) -> Result<Json<wyrd_doctor::Report>, Problem> {
    let bin = s.wyrd_bin.clone();
    let report = tokio::task::spawn_blocking(move || doctor_report(&bin))
        .await
        .map_err(|e| Problem::internal(e.to_string()))?;
    Ok(Json(report))
}

pub async fn metrics(State(s): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        s.metrics.render(),
    )
}
