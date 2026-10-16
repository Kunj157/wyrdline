//! Mutating session operations: stop, checkpoint, rewind, fork, diff,
//! apply. Each writes its ledger event through `wyrd-cell`, as the CLI does.

use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use serde::de::DeserializeOwned;

use wyrd_cell::state::CheckpointRecord;

use super::sessions::{blocking, open};
use crate::AppState;
use crate::api::{
    ApplyBody, ApplyResult, BranchCreated, Changes, ForkBody, Relaunching, RewindBody, Stopped,
};
use crate::problem::Problem;

/// A JSON request body; an empty body reads as `{}`. Malformed bodies and
/// unknown fields answer 422 problem+json.
#[derive(Debug)]
pub struct JsonBody<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for JsonBody<T> {
    type Rejection = Problem;

    async fn from_request(req: Request, state: &S) -> Result<Self, Problem> {
        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            Problem::new(StatusCode::BAD_REQUEST, "invalid-body", "Unreadable body")
                .detail(e.body_text())
        })?;
        let bytes: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
            b"{}"
        } else {
            &bytes
        };
        serde_json::from_slice(bytes).map(JsonBody).map_err(|e| {
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid-body",
                "Invalid request body",
            )
            .detail(e.to_string())
        })
    }
}

/// Checkpoint ids, branch names, and `head`: short, no path characters.
fn check_target(t: &str) -> Result<(), Problem> {
    let ok = !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid-target",
            "Malformed checkpoint or branch",
        ))
    }
}

pub async fn stop(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<Stopped>, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let mut s = open(&data, &sid)?;
        Ok(Json(Stopped { stopped: s.stop()? }))
    })
    .await
}

pub async fn checkpoint(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<(StatusCode, Json<CheckpointRecord>), Problem> {
    let data = st.data.clone();
    blocking(move || {
        let mut s = open(&data, &sid)?;
        Ok((StatusCode::CREATED, Json(s.checkpoint(None)?)))
    })
    .await
}

/// Hand a rewind or fork with relaunch to a detached `wyrd`, which owns
/// live sessions. Output goes to `relaunch.log` in the session directory.
fn spawn_relaunch(st: &AppState, sid: &str, args: &[&str]) -> Result<u32, Problem> {
    use std::os::unix::process::CommandExt as _;
    let s = open(&st.data, sid)?;
    let log_path = s.dir().root().join("relaunch.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| Problem::internal(format!("{}: {e}", log_path.display())))?;
    let err = log
        .try_clone()
        .map_err(|e| Problem::internal(e.to_string()))?;
    let mut child = Command::new(&st.wyrd_bin)
        .args(args)
        .env("WYRD_DATA_DIR", st.data.root())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .process_group(0)
        .spawn()
        .map_err(|e| Problem::internal(format!("starting {}: {e}", st.wyrd_bin.display())))?;
    let pid = child.id();
    // Reap it when it exits; the daemon does not wait for the agent.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

pub async fn rewind(
    State(st): State<Arc<AppState>>,
    Path((sid, target)): Path<(String, String)>,
    JsonBody(body): JsonBody<RewindBody>,
) -> Result<Response, Problem> {
    check_target(&target)?;
    let relaunch = body.relaunch;
    blocking(move || {
        if relaunch {
            let s = open(&st.data, &sid)?;
            s.resolve(&target)?;
            let id = s.record().id.clone();
            let pid = spawn_relaunch(&st, &id, &["rewind", &id, &target, "--relaunch"])?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(Relaunching {
                    relaunching: true,
                    pid,
                }),
            )
                .into_response());
        }
        let mut s = open(&st.data, &sid)?;
        let b = s.rewind(&target, None)?;
        Ok((StatusCode::OK, Json(BranchCreated { branch: b })).into_response())
    })
    .await
}

pub async fn fork(
    State(st): State<Arc<AppState>>,
    Path((sid, target)): Path<(String, String)>,
    JsonBody(body): JsonBody<ForkBody>,
) -> Result<Response, Problem> {
    check_target(&target)?;
    check_target(&body.name)?;
    blocking(move || {
        let mut s = open(&st.data, &sid)?;
        if body.relaunch {
            s.resolve(&target)?;
            if s.record().branch(&body.name).is_some() {
                return Err(
                    Problem::new(StatusCode::BAD_REQUEST, "invalid", "Invalid request")
                        .detail(format!("branch {} already exists", body.name)),
                );
            }
            let id = s.record().id.clone();
            let pid = spawn_relaunch(
                &st,
                &id,
                &["fork", &id, &target, "--name", &body.name, "--relaunch"],
            )?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(Relaunching {
                    relaunching: true,
                    pid,
                }),
            )
                .into_response());
        }
        let b = s.rewind(&target, Some(&body.name))?;
        Ok((StatusCode::CREATED, Json(BranchCreated { branch: b })).into_response())
    })
    .await
}

pub async fn diff(
    State(st): State<Arc<AppState>>,
    Path((sid, a, b)): Path<(String, String, String)>,
) -> Result<Json<Changes>, Problem> {
    check_target(&a)?;
    check_target(&b)?;
    let data = st.data.clone();
    blocking(move || {
        let s = open(&data, &sid)?;
        Ok(Json(Changes {
            changes: s.diff(&a, &b)?,
        }))
    })
    .await
}

pub async fn apply(
    State(st): State<Arc<AppState>>,
    Path((sid, target)): Path<(String, String)>,
    JsonBody(body): JsonBody<ApplyBody>,
) -> Result<Json<ApplyResult>, Problem> {
    check_target(&target)?;
    let data = st.data.clone();
    blocking(move || {
        let mut s = open(&data, &sid)?;
        let report = s.apply(&target, body.dry_run, body.force)?;
        if !report.conflicts.is_empty() && !report.applied && !body.dry_run {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "workspace-changed",
                "The real workspace changed since the session started",
            )
            .detail(report.conflicts.join(", ")));
        }
        Ok(Json(report))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_have_no_path_characters() {
        for ok in ["c1", "head", "my-branch", "b_2"] {
            assert!(check_target(ok).is_ok(), "{ok}");
        }
        for bad in ["", "../c1", "a/b", "a b", &"x".repeat(65)] {
            assert!(check_target(bad).is_err(), "{bad}");
        }
    }
}
