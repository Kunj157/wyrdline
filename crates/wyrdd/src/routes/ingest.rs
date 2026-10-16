//! Intent from outside the agent's hooks: hook payloads and OTLP/HTTP JSON
//! trace exports, forwarded to the live session's intent socket (the
//! session's single ledger writer is `wyrd run`).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde_json::Value;
use wyrd_adapters::{Adapter, Request, Response};

use super::sessions::{blocking, open};
use crate::AppState;
use crate::api::{OtlpExportResponse, PartialSuccess};
use crate::problem::Problem;

/// Largest accepted ingest body.
pub const MAX_BODY: usize = 8 * 1024 * 1024;
/// How long the session may take to answer.
pub const TIMEOUT: Duration = Duration::from_secs(5);

fn not_live() -> Problem {
    Problem::new(
        StatusCode::CONFLICT,
        "session-not-live",
        "The session is not recording; nothing listens on its intent socket",
    )
}

/// Send `req` to the intent socket of session `sid`.
async fn forward(st: &AppState, sid: String, req: Request) -> Result<Response, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let s = open(&data, &sid)?;
        let socket = wyrd_adapters::server::socket_path(s.dir().root(), &s.record().id);
        if !socket.exists() {
            return Err(not_live());
        }
        wyrd_adapters::protocol::call(&socket, &req, TIMEOUT).map_err(|e| match e {
            wyrd_adapters::protocol::ProtocolError::Io(_) => not_live(),
            other => Problem::internal(other.to_string()),
        })
    })
    .await
}

fn json_body(headers: &HeaderMap, body: &Bytes) -> Result<Value, Problem> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");
    if ct.starts_with("application/x-protobuf") || ct.starts_with("application/protobuf") {
        return Err(Problem::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "protobuf-unsupported",
            "Only OTLP/HTTP JSON is accepted",
        )
        .detail("set OTEL_EXPORTER_OTLP_PROTOCOL=http/json"));
    }
    serde_json::from_slice(body).map_err(|e| {
        Problem::new(StatusCode::BAD_REQUEST, "invalid-body", "Body is not JSON")
            .detail(e.to_string())
    })
}

fn session_error(r: Response) -> Result<Response, Problem> {
    match r {
        Response::Error { message } => Err(Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "rejected",
            "The session rejected the input",
        )
        .detail(message)),
        other => Ok(other),
    }
}

/// `POST /v1/sessions/{sid}/otlp/v1/traces`. Answers like an OTLP
/// collector: an empty `ExportTraceServiceResponse`.
pub async fn traces(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<OtlpExportResponse>, Problem> {
    let request = json_body(&headers, &body)?;
    session_error(forward(&st, sid, Request::Otlp { request }).await?)?;
    Ok(Json(OtlpExportResponse {
        partial_success: PartialSuccess {},
    }))
}

/// `POST /v1/sessions/{sid}/ingest/hooks/{adapter}`. Answers with the
/// session's response (a decision for pre-execution hooks).
pub async fn hook(
    State(st): State<Arc<AppState>>,
    Path((sid, adapter)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Response>, Problem> {
    let Some(adapter) = Adapter::from_name(&adapter) else {
        return Err(Problem::not_found(format!("adapter {adapter}")));
    };
    let payload = json_body(&headers, &body)?;
    let r = forward(
        &st,
        sid,
        Request::Hook {
            adapter: adapter.name().to_string(),
            payload,
        },
    )
    .await?;
    Ok(Json(session_error(r)?))
}
