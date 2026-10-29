//! RFC 9457 problem details: the body of every error response.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// An error answered as `application/problem+json`. `detail` is for the
/// caller and never carries internal error text (that goes to the log).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// A stable identifier of the problem kind, e.g. `not-found`.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Extension members (RFC 9457 §3.2).
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

impl Problem {
    pub fn new(status: StatusCode, kind: &str, title: &str) -> Self {
        Problem {
            kind: kind.into(),
            title: title.into(),
            status: status.as_u16(),
            detail: None,
            extensions: serde_json::Map::new(),
        }
    }

    /// Add an extension member.
    #[must_use]
    pub fn with(mut self, key: &str, value: serde_json::Value) -> Self {
        self.extensions.insert(key.into(), value);
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn not_found() -> Self {
        Problem::new(StatusCode::NOT_FOUND, "not-found", "Not found")
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Problem::new(StatusCode::BAD_REQUEST, "bad-request", "Bad request").detail(detail)
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Service unavailable",
        )
        .detail(detail)
    }

    /// Log `err` and answer 500 without its text.
    pub fn internal(err: &dyn std::fmt::Display) -> Self {
        tracing::error!(error = %err, "internal error");
        Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error",
        )
    }
}

impl From<sqlx_core::Error> for Problem {
    fn from(e: sqlx_core::Error) -> Self {
        match e {
            sqlx_core::Error::PoolTimedOut
            | sqlx_core::Error::PoolClosed
            | sqlx_core::Error::Io(_) => {
                tracing::error!(error = %e, "database unavailable");
                Problem::unavailable("the database is unavailable")
            }
            e => Problem::internal(&e),
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut res = (status, Json(self)).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        res
    }
}
