//! `GET /api/v1/orgs/{org}/stats` and `GET /api/v1/orgs/{org}/search`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access;
use crate::auth::Caller;
use crate::db;
use crate::projection;
use crate::routes::bundles::BundleRow;
use crate::{AppState, Problem};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/orgs/{org}/stats", get(stats))
        .route("/api/v1/orgs/{org}/search", get(search))
}

/// Bundles considered by one query (most recent first).
const MAX_BUNDLES: i64 = 500;

#[derive(Debug, Deserialize)]
struct StatsQuery {
    /// RFC 3339 time or date: bundles uploaded at or after.
    since: Option<String>,
}

fn bad_since(e: sqlx_core::Error) -> Problem {
    let input = e
        .as_database_error()
        .and_then(sqlx_core::error::DatabaseError::code)
        .is_some_and(|c| c.starts_with("22"));
    if input {
        Problem::bad_request("since: expected an RFC 3339 time or a date")
    } else {
        e.into()
    }
}

async fn stats(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    let started = std::time::Instant::now();
    let by_repo: Vec<(Option<String>, i64)> = db::query_as(
        "SELECT repo, count(*) FROM bundles WHERE org_id = $1 \
         AND ($2::text IS NULL OR created_at >= ($2::text)::timestamptz) \
         GROUP BY repo ORDER BY count(*) DESC, repo",
    )
    .bind(&a.org_id)
    .bind(&q.since)
    .fetch_all(pool)
    .await
    .map_err(bad_since)?;
    let by_agent: Vec<(String, i64)> = db::query_as(
        "SELECT agent, count(*) FROM bundles WHERE org_id = $1 \
         AND ($2::text IS NULL OR created_at >= ($2::text)::timestamptz) \
         GROUP BY agent ORDER BY count(*) DESC, agent",
    )
    .bind(&a.org_id)
    .bind(&q.since)
    .fetch_all(pool)
    .await?;
    let total: i64 = by_repo.iter().map(|(_, n)| n).sum();
    let rows: Vec<BundleRow> = db::query_as(crate::routes::bundles::select!(
        " WHERE b.org_id = $1 AND ($2::text IS NULL OR b.created_at >= ($2::text)::timestamptz) \
          ORDER BY b.id DESC LIMIT $3"
    ))
    .bind(&a.org_id)
    .bind(&q.since)
    .bind(MAX_BUNDLES)
    .fetch_all(pool)
    .await?;
    let s = engine::stats(&st, &rows).await?;
    st.metrics.observe(
        "wyrd_hub_query_seconds",
        &[("query", "stats")],
        started.elapsed(),
    );
    let considered = rows.len();
    #[allow(clippy::cast_precision_loss)] // counts far below 2^52
    let rate = |n: i64| {
        if considered == 0 {
            0.0
        } else {
            n as f64 / considered as f64
        }
    };
    Ok(Json(json!({
        "sessions": {
            "total": total,
            "considered": considered,
            "by_repo": by_repo.iter().map(|(r, n)| json!({"repo": r, "sessions": n})).collect::<Vec<_>>(),
            "by_agent": by_agent.iter().map(|(r, n)| json!({"agent": r, "sessions": n})).collect::<Vec<_>>(),
        },
        "divergences": {
            "by_rule": s.rules.iter().map(|(rule, sessions, raised)| json!({
                "rule": rule, "sessions": sessions, "raised": raised, "rate": rate(*sessions),
            })).collect::<Vec<_>>(),
        },
        "denied": {
            "total": s.denied.iter().map(|(_, n)| n).sum::<i64>(),
            "by_kind": s.denied.iter().map(|(k, n)| json!({"kind": k, "count": n})).collect::<Vec<_>>(),
        },
        "capture": {
            "events": s.events,
            "dropped_events": s.dropped,
            "sensor_lost": s.sensor_lost,
            // Not recorded in bundle format v1.
            "overhead_pct": Value::Null,
        },
        // Judge runs are kept next to the local ledger, not in bundles
        // (format v1), so the Hub has none to count yet.
        "judge": { "recorded": false, "runs": 0, "input_tokens": 0, "output_tokens": 0, "cost_usd": 0.0 },
    })))
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<i64>,
}

async fn search(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
    Query(sq): Query<SearchQuery>,
) -> Result<Json<Value>, Problem> {
    let parsed = projection::parse(&sq.q).map_err(|e| {
        Problem::bad_request(e.to_string())
            .with("position", json!(e.position))
            .with("reason", json!(e.message))
    })?;
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    let started = std::time::Instant::now();
    let limit = sq.limit.unwrap_or(100).clamp(1, 1000);
    let rows: Vec<BundleRow> = db::query_as(crate::routes::bundles::select!(
        " WHERE b.org_id = $1 \
          AND (cardinality($2::text[]) = 0 OR b.repo = ANY ($2)) \
          AND b.rules @> $3::text[] \
          AND (cardinality($4::text[]) = 0 OR b.agent = ANY ($4)) \
          AND (cardinality($5::text[]) = 0 OR b.session_id = ANY ($5)) \
          ORDER BY b.id DESC LIMIT $6"
    ))
    .bind(&a.org_id)
    .bind(&parsed.repos)
    .bind(&parsed.rules)
    .bind(&parsed.agents)
    .bind(&parsed.sessions)
    .bind(MAX_BUNDLES)
    .fetch_all(pool)
    .await?;
    let hits = if parsed.needs_events() {
        engine::search(&st, &rows, &parsed, limit).await?
    } else {
        Vec::new()
    };
    st.metrics.observe(
        "wyrd_hub_query_seconds",
        &[("query", "search")],
        started.elapsed(),
    );
    let by_id = |id: &str| rows.iter().find(|r| r.id == id);
    let hits: Vec<Value> = hits
        .into_iter()
        .map(|(bundle_id, seq, kind, path, target, detail)| {
            let b = by_id(&bundle_id);
            json!({
                "bundle": bundle_id,
                "session": b.map(|b| b.session_name.clone().unwrap_or_else(|| b.session_id.clone())),
                "repo": b.and_then(|b| b.repo.clone()),
                "seq": seq, "kind": kind, "path": path, "target": target, "detail": detail,
            })
        })
        .collect();
    let sessions: Vec<Value> = rows
        .iter()
        .take(usize::try_from(limit).unwrap_or(100))
        .map(BundleRow::json)
        .collect();
    Ok(Json(json!({
        "sessions": sessions,
        "hits": hits,
        "events_searched": parsed.needs_events(),
    })))
}

/// A hit: bundle id, seq, kind, path, target, detail.
type HitRow = (
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[cfg(feature = "analytics")]
mod engine {
    use std::path::PathBuf;

    use super::HitRow;
    use crate::analytics::{self, Stats};
    use crate::projection::{self, Query};
    use crate::routes::bundles::BundleRow;
    use crate::{AppState, Problem, ingest};

    fn failed(e: &dyn std::fmt::Display) -> Problem {
        Problem::internal(e)
    }

    /// The projection of `row`, rebuilt from the stored bundle if missing.
    pub async fn ensure(st: &AppState, row: &BundleRow) -> Result<PathBuf, Problem> {
        let dir = analytics::dir(&st.config.data_dir, &row.org_id, &row.id);
        if analytics::exists(&dir) {
            return Ok(dir);
        }
        let obj = st.storage.get(&row.storage_key).await.map_err(|e| {
            tracing::error!(error = %e, bundle = row.id, "projection rebuild: object storage");
            Problem::unavailable("object storage is unavailable")
        })?;
        let spool = ingest::Spool::create(&st.config.data_dir).await?;
        let path = spool.path.clone();
        let mut reader = obj.reader;
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&path)?;
            std::io::copy(&mut reader, &mut f)?;
            Ok(())
        })
        .await
        .map_err(|e| failed(&e))?
        .map_err(|e| failed(&e))?;
        let bundle = ingest::verify(spool.path.clone(), st.config.limits.max_upload_bytes).await?;
        let rows = projection::rows(&bundle);
        drop(bundle);
        let out = dir.clone();
        tokio::task::spawn_blocking(move || analytics::write(&rows, &out))
            .await
            .map_err(|e| failed(&e))?
            .map_err(|e| failed(&e))?;
        st.metrics.inc("wyrd_hub_projection_rebuilds_total", &[]);
        tracing::info!(bundle = row.id, "projection rebuilt");
        Ok(dir)
    }

    async fn dirs(st: &AppState, rows: &[BundleRow]) -> Result<Vec<PathBuf>, Problem> {
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ensure(st, r).await?);
        }
        Ok(out)
    }

    pub async fn stats(st: &AppState, rows: &[BundleRow]) -> Result<Stats, Problem> {
        let dirs = dirs(st, rows).await?;
        tokio::task::spawn_blocking(move || analytics::stats(&dirs))
            .await
            .map_err(|e| failed(&e))?
            .map_err(|e| failed(&e))
    }

    pub async fn search(
        st: &AppState,
        rows: &[BundleRow],
        q: &Query,
        limit: i64,
    ) -> Result<Vec<HitRow>, Problem> {
        let dirs = dirs(st, rows).await?;
        let q = q.clone();
        let hits = tokio::task::spawn_blocking(move || analytics::search(&dirs, &q, limit))
            .await
            .map_err(|e| failed(&e))?
            .map_err(|e| failed(&e))?;
        Ok(hits
            .into_iter()
            .map(|h| (h.bundle_id, h.seq, h.kind, h.path, h.target, h.detail))
            .collect())
    }
}

#[cfg(not(feature = "analytics"))]
#[allow(clippy::unused_async)] // same signatures as the real engine
mod engine {
    use axum::http::StatusCode;

    use super::HitRow;
    use crate::projection::Query;
    use crate::routes::bundles::BundleRow;
    use crate::{AppState, Problem};

    /// What [`crate::analytics::stats`] returns, when it is built in.
    #[derive(Debug, Default)]
    pub struct Stats {
        pub rules: Vec<(String, i64, i64)>,
        pub denied: Vec<(String, i64)>,
        pub events: i64,
        pub dropped: i64,
        pub sensor_lost: i64,
    }

    fn disabled() -> Problem {
        Problem::new(
            StatusCode::NOT_IMPLEMENTED,
            "analytics-disabled",
            "This Hub was built without analytics",
        )
    }

    pub async fn stats(_: &AppState, _: &[BundleRow]) -> Result<Stats, Problem> {
        Err(disabled())
    }

    pub async fn search(
        _: &AppState,
        _: &[BundleRow],
        _: &Query,
        _: i64,
    ) -> Result<Vec<HitRow>, Problem> {
        Err(disabled())
    }
}

/// Project a just-uploaded bundle; a failure only logs (the projection
/// is rebuilt on first use).
#[cfg(feature = "analytics")]
pub async fn project_upload(st: &AppState, org_id: &str, bundle_id: &str, rows: projection::Rows) {
    let dir = crate::analytics::dir(&st.config.data_dir, org_id, bundle_id);
    match tokio::task::spawn_blocking(move || crate::analytics::write(&rows, &dir)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, bundle_id, "projection not written"),
        Err(e) => tracing::warn!(error = %e, bundle_id, "projection task failed"),
    }
}
