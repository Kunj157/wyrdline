//! A session's divergences, and their triage (acknowledge / false
//! positive), which is written to the ledger as `verdict.divergence.updated`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use wyrd_divergence::view::{DivergenceView, project};
use wyrd_schema::{DivergenceUpdated, EventBody};

use super::actions::JsonBody;
use super::sessions::{all_events, blocking, open};
use crate::AppState;
use crate::api::{DivergenceList, TriageBody};
use crate::problem::Problem;

/// Longest triage note accepted.
pub const MAX_NOTE: usize = 2_000;

pub async fn list(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<DivergenceList>, Problem> {
    blocking(move || {
        let s = open(&st.data, &sid)?;
        Ok(Json(DivergenceList {
            divergences: project(&all_events(&s)?),
        }))
    })
    .await
}

pub async fn triage(
    State(st): State<Arc<AppState>>,
    Path((sid, did)): Path<(String, String)>,
    JsonBody(body): JsonBody<TriageBody>,
) -> Result<Json<DivergenceView>, Problem> {
    if body
        .note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE)
    {
        return Err(Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-body",
            "Note too long",
        ));
    }
    let metrics = st.clone();
    let view = blocking(move || {
        let mut s = open(&st.data, &sid)?;
        if !project(&all_events(&s)?)
            .iter()
            .any(|v| v.divergence.divergence_id == did)
        {
            return Err(Problem::not_found(format!("divergence {did}")));
        }
        s.record_verdicts(vec![EventBody::DivergenceUpdated(DivergenceUpdated {
            divergence_id: did.clone(),
            status: body.status,
            note: body.note,
        })])?;
        project(&all_events(&s)?)
            .into_iter()
            .find(|v| v.divergence.divergence_id == did)
            .ok_or_else(|| Problem::internal("divergence vanished after triage"))
    })
    .await?;
    metrics.metrics.triage();
    Ok(Json(view))
}
