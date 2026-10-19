//! Policy components for backtests: uploaded into `$DATA/policies`, then
//! replayed against recorded sessions (plan §14.1). Uploaded components are
//! untrusted code and run in the same sandbox as at run time (ADR-007).

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;

use super::sessions::{blocking, open};
use crate::AppState;
use crate::api::{BacktestBody, BacktestFinding, BacktestReport, PolicyInfo, PolicyList};
use crate::problem::Problem;

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn info(id: &str, bytes: &[u8]) -> PolicyInfo {
    PolicyInfo {
        id: id.to_string(),
        version: wyrd_policy::version_of(bytes),
        bytes: bytes.len() as u64,
    }
}

pub async fn list(State(st): State<Arc<AppState>>) -> Result<Json<PolicyList>, Problem> {
    blocking(move || {
        let mut policies = Vec::new();
        if let Ok(dir) = std::fs::read_dir(st.data.policies()) {
            for e in dir.flatten() {
                let path = e.path();
                let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|_| path.extension().is_some_and(|x| x == "wasm"))
                else {
                    continue;
                };
                if let Ok(bytes) = std::fs::read(&path) {
                    policies.push(info(id, &bytes));
                }
            }
        }
        policies.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Json(PolicyList { policies }))
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    name: String,
}

pub async fn upload(
    State(st): State<Arc<AppState>>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<PolicyInfo>), Problem> {
    if !valid_id(&q.name) {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid-name",
            "Policy names are 1–64 letters, digits, - and _",
        ));
    }
    blocking(move || {
        // Compile it once: a component that does not implement the policy
        // world is refused before it is stored.
        wyrd_policy::Policy::load(&q.name, &body).map_err(|e| {
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid-policy",
                "Not a wyrdline policy component",
            )
            .detail(e.to_string())
        })?;
        let dir = st.data.policies();
        std::fs::create_dir_all(&dir).map_err(|e| Problem::internal(e.to_string()))?;
        let tmp = dir.join(format!(".{}.wasm.tmp", q.name));
        std::fs::write(&tmp, &body)
            .and_then(|()| std::fs::rename(&tmp, dir.join(format!("{}.wasm", q.name))))
            .map_err(|e| Problem::internal(e.to_string()))?;
        Ok((StatusCode::CREATED, Json(info(&q.name, &body))))
    })
    .await
}

pub async fn backtest(
    State(st): State<Arc<AppState>>,
    Path(pid): Path<String>,
    Json(body): Json<BacktestBody>,
) -> Result<Json<BacktestReport>, Problem> {
    if !valid_id(&pid) {
        return Err(Problem::not_found(format!("no policy {pid}")));
    }
    blocking(move || {
        let path = st.data.policies().join(format!("{pid}.wasm"));
        if !path.is_file() {
            return Err(Problem::not_found(format!("no policy {pid}")));
        }
        let policy = wyrd_policy::Policy::load_file(&path).map_err(|e| {
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid-policy",
                "The stored policy no longer loads",
            )
            .detail(e.to_string())
        })?;
        let s = open(&st.data, &body.session_id)?;
        let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())
            .map_err(|e| Problem::internal(e.to_string()))?;
        let events = ledger
            .events_after(0, usize::MAX)
            .map_err(|e| Problem::internal(e.to_string()))?;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let r = wyrd_policy::backtest::backtest(
            &policy,
            &events,
            &s.record().workspace,
            home.as_deref(),
        );
        st.metrics.backtest();
        Ok(Json(BacktestReport {
            policy_id: r.policy_id,
            policy_version: r.policy_version,
            session_id: s.record().id.clone(),
            events: r.events,
            findings: r
                .findings
                .into_iter()
                .map(|f| BacktestFinding {
                    seq: f.seq,
                    stage: f.stage,
                    decision: f.decision,
                    reason: f.reason,
                })
                .collect(),
            notes: r.notes,
        }))
    })
    .await
}
