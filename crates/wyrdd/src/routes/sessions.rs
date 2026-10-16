//! Read-only views of recorded sessions.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wyrd_cell::{CellError, DataDir, Session};

use crate::AppState;
use crate::problem::Problem;

/// Default and maximum page size for `/events`.
pub const DEFAULT_LIMIT: usize = 500;
pub const MAX_LIMIT: usize = 5_000;

impl From<CellError> for Problem {
    fn from(e: CellError) -> Self {
        match e {
            CellError::NotFound(what) => Problem::not_found(what),
            CellError::Invalid(why) if why.contains("ambiguous") => {
                Problem::new(StatusCode::CONFLICT, "ambiguous", "Ambiguous id").detail(why)
            }
            CellError::Invalid(why) => {
                Problem::new(StatusCode::BAD_REQUEST, "invalid", "Invalid request").detail(why)
            }
            CellError::Conflicts(paths) => Problem::new(
                StatusCode::CONFLICT,
                "workspace-changed",
                "The real workspace changed since the session started",
            )
            .detail(paths.join(", ")),
            other => Problem::internal(other.to_string()),
        }
    }
}

/// Session ids are lowercase ULIDs; a prefix is accepted as in the CLI.
pub fn check_id(id: &str) -> Result<(), Problem> {
    if !id.is_empty()
        && id.len() <= 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    {
        Ok(())
    } else {
        Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid-id",
            "Malformed session id",
        ))
    }
}

/// Run blocking file and ledger work off the async runtime.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Problem> + Send + 'static,
) -> Result<T, Problem> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Problem::internal(e.to_string()))?
}

pub fn open(data: &DataDir, sid: &str) -> Result<Session, Problem> {
    check_id(sid)?;
    Ok(Session::open(data, sid)?)
}

impl From<wyrd_ledger::LedgerError> for Problem {
    fn from(e: wyrd_ledger::LedgerError) -> Self {
        Problem::internal(e.to_string())
    }
}

fn to_json(v: &impl Serialize) -> Result<Value, Problem> {
    serde_json::to_value(v).map_err(|e| Problem::internal(e.to_string()))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    agent: Option<String>,
    /// Workspace path prefix.
    repo: Option<String>,
    /// Created at or after, ms since the epoch.
    since: Option<i64>,
}

pub async fn list(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let mut out = Vec::new();
        for r in Session::list(&data)? {
            let v = to_json(&r)?;
            if q.status.as_deref().is_some_and(|s| v["status"] != s)
                || q.agent.as_deref().is_some_and(|a| r.agent != a)
                || q.repo
                    .as_deref()
                    .is_some_and(|p| !r.workspace.starts_with(p))
                || q.since.is_some_and(|t| r.created_ms < t)
            {
                continue;
            }
            out.push(v);
        }
        Ok(Json(json!({ "sessions": out })))
    })
    .await
}

pub async fn get(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let s = open(&data, &sid)?;
        let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
        let (head_seq, head_hash) = ledger.head();
        let mut v = to_json(s.record())?;
        v["head_seq"] = json!(head_seq);
        v["head_hash"] = json!(format!("b3:{}", hex::encode(head_hash)));
        v["sealed"] = json!(ledger.is_sealed());
        Ok(Json(v))
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    plane: Option<String>,
    /// Exact kind, or a dotted prefix such as `effect.file`.
    kind: Option<String>,
    after_seq: Option<u64>,
    limit: Option<usize>,
}

fn kind_matches(kind: &str, filter: &str) -> bool {
    kind == filter
        || kind
            .strip_prefix(filter)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub async fn events(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Value>, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let s = open(&data, &sid)?;
        let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut after = q.after_seq.unwrap_or(0);
        let mut out = Vec::new();
        // Scan forward until the page is full or the ledger ends; `after`
        // always advances past every event examined, filtered or not.
        'scan: loop {
            let batch = ledger.events_after(after, MAX_LIMIT)?;
            if batch.is_empty() {
                break;
            }
            for ev in batch {
                after = ev.seq;
                let keep = q.plane.as_deref().is_none_or(|p| ev.plane().as_str() == p)
                    && q.kind.as_deref().is_none_or(|k| kind_matches(ev.kind(), k));
                if keep {
                    out.push(to_json(&ev)?);
                    if out.len() == limit {
                        break 'scan;
                    }
                }
            }
        }
        let (head, _) = ledger.head();
        let next = (after < head).then_some(after);
        Ok(Json(json!({ "events": out, "next_after_seq": next })))
    })
    .await
}

pub async fn verify(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let s = open(&data, &sid)?;
        let v = wyrd_ledger::verify(&s.dir().ledger())?;
        let mut out = to_json(&v)?;
        out["ok"] = json!(v.ok());
        Ok(Json(out))
    })
    .await
}

pub async fn checkpoints(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, Problem> {
    let data = st.data.clone();
    blocking(move || {
        let s = open(&data, &sid)?;
        let r = s.record();
        Ok(Json(json!({
            "current_branch": r.current_branch,
            "branches": to_json(&r.branches)?,
            "checkpoints": to_json(&r.checkpoints)?,
        })))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_lowercase_alphanumeric() {
        assert!(check_id("01jabc").is_ok());
        for bad in ["", "../x", "01JABC", "a/b", "a.b", &"a".repeat(27)] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn kind_filter_matches_whole_segments() {
        assert!(kind_matches("effect.file.write", "effect.file"));
        assert!(kind_matches("effect.file.write", "effect.file.write"));
        assert!(!kind_matches("effect.filesystem", "effect.file"));
    }
}
