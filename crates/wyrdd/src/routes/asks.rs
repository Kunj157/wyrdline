//! Pending asks of a live session, for Studio: listed and answered through
//! the session's intent socket with its control token (the daemon runs as
//! the session's user and can read it; the agent's Cell cannot).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use wyrd_adapters::asks::CONTROL_TOKEN_FILE;
use wyrd_adapters::{Request, Response};

use super::sessions::{blocking, open};
use crate::AppState;
use crate::api::{AnswerBody, Answered, PendingAsks};
use crate::problem::Problem;

/// The control token of a running session, or `None` when it is not running.
fn control(st: &AppState, sid: &str) -> Result<Option<(std::path::PathBuf, String)>, Problem> {
    let s = open(&st.data, sid)?;
    let id = s.record().id.clone();
    match std::fs::read_to_string(s.dir().root().join(CONTROL_TOKEN_FILE)) {
        Ok(token) => Ok(Some((
            wyrd_adapters::server::socket_path(s.dir().root(), &id),
            token.trim().to_string(),
        ))),
        Err(_) => Ok(None),
    }
}

fn call(socket: &std::path::Path, req: &Request) -> Result<Option<Response>, Problem> {
    match wyrd_adapters::protocol::call(socket, req, super::ingest::TIMEOUT) {
        Ok(r) => Ok(Some(r)),
        // The session ended mid-request: an I/O error or a connection closed
        // without an answer. Same as no session.
        Err(
            wyrd_adapters::protocol::ProtocolError::Io(_)
            | wyrd_adapters::protocol::ProtocolError::NoResponse,
        ) => Ok(None),
        Err(e) => Err(Problem::internal(e.to_string())),
    }
}

pub async fn list(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<PendingAsks>, Problem> {
    blocking(move || {
        let Some((socket, token)) = control(&st, &sid)? else {
            return Ok(Json(PendingAsks { asks: Vec::new() }));
        };
        match call(&socket, &Request::Asks { token })? {
            Some(Response::Asks { asks }) => Ok(Json(PendingAsks { asks })),
            // The session refused the token it was read with, or is still
            // starting. Nothing is waiting for an answer right now.
            None | Some(Response::Error { .. }) => Ok(Json(PendingAsks { asks: Vec::new() })),
            Some(other) => Err(Problem::internal(format!("unexpected answer {other:?}"))),
        }
    })
    .await
}

pub async fn answer(
    State(st): State<Arc<AppState>>,
    Path((sid, id)): Path<(String, u64)>,
    Json(body): Json<AnswerBody>,
) -> Result<Json<Answered>, Problem> {
    blocking(move || {
        let gone = || Problem::not_found(format!("no pending ask {id}"));
        let Some((socket, token)) = control(&st, &sid)? else {
            return Err(gone());
        };
        let req = Request::Answer {
            token,
            id,
            allow: body.allow,
            by: "studio".into(),
        };
        match call(&socket, &req)? {
            Some(Response::Ok) => Ok(Json(Answered {
                id,
                allow: body.allow,
            })),
            Some(Response::Error { .. }) | None => Err(gone()),
            Some(other) => Err(Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Unexpected answer from the session",
            )
            .detail(format!("{other:?}"))),
        }
    })
    .await
}
