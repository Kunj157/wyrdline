//! RFC 9457 problem details: the body of every error response.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// An error answered as `application/problem+json`.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    /// A stable, relative identifier of the problem kind, e.g. `unauthorized`.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Problem {
    pub fn new(status: StatusCode, kind: &str, title: &str) -> Self {
        Problem {
            kind: kind.into(),
            title: title.into(),
            status: status.as_u16(),
            detail: None,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn unauthorized() -> Self {
        Problem::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        )
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Problem::new(StatusCode::NOT_FOUND, "not-found", "Not found").detail(detail)
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error",
        )
        .detail(detail)
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

/// Unknown routes answer with a problem, too.
pub async fn fallback() -> Problem {
    Problem::not_found("no such endpoint")
}
