//! Rewind requests the agent made through `wyrd mcp`, for Studio: listed
//! from the ledger and decided by the user. These routes need the user's
//! daemon token like every other `/v1` route; the agent's Cell can read
//! neither that token nor the session's control token.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};

use super::actions::JsonBody;
use super::sessions::{blocking, open};
use crate::AppState;
use crate::api::{RewindDecided, RewindDecisionBody, RewindRequests};
use crate::problem::Problem;

pub async fn list(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<RewindRequests>, Problem> {
    blocking(move || {
        let s = open(&st.data, &sid)?;
        Ok(Json(RewindRequests {
            requests: s.rewind_requests()?,
        }))
    })
    .await
}

pub async fn decide(
    State(st): State<Arc<AppState>>,
    Path((sid, id)): Path<(String, u64)>,
    JsonBody(body): JsonBody<RewindDecisionBody>,
) -> Result<Json<RewindDecided>, Problem> {
    blocking(move || {
        let mut s = open(&st.data, &sid)?;
        let branch = if body.confirm {
            Some(s.confirm_rewind(id, "studio")?)
        } else {
            s.reject_rewind(id, "studio", body.note)?;
            None
        };
        let request = s
            .rewind_requests()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| Problem::not_found(format!("rewind request {id}")))?;
        Ok(Json(RewindDecided { request, branch }))
    })
    .await
}
