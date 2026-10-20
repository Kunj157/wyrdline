//! Bundles: upload (streamed, verified, quota-checked), list, metadata,
//! download, delete, and the organisation's retention setting.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx_core::from_row::FromRow;
use sqlx_core::row::Row as _;
use tokio::io::AsyncWriteExt as _;

use crate::access::{self, OrgAccess};
use crate::audit::{self, Entry};
use crate::auth::{Caller, Role};
use crate::db::{self, PgRow};
use crate::ingest::{self, Spool};
use crate::storage::StorageError;
use crate::{AppState, Problem};

pub fn router(max_upload_bytes: u64) -> Router<Arc<AppState>> {
    // Room for the multipart framing and the small text fields.
    let body_limit =
        usize::try_from(max_upload_bytes.saturating_add(1 << 20)).unwrap_or(usize::MAX);
    Router::new()
        .route(
            "/api/v1/orgs/{org}/bundles",
            get(list)
                .post(upload)
                .layer(DefaultBodyLimit::max(body_limit)),
        )
        .route("/api/v1/orgs/{org}", patch(update_org))
        .route("/api/v1/bundles/{bid}", get(show).delete(remove))
        .route("/api/v1/bundles/{bid}/download", get(download))
}

/// One `bundles` row as the API shows it.
#[derive(Debug, Clone)]
pub struct BundleRow {
    pub id: String,
    pub org_id: String,
    pub org: String,
    pub repo: Option<String>,
    pub session_id: String,
    pub session_name: Option<String>,
    pub agent: String,
    pub session_created_ms: Option<i64>,
    pub events: i64,
    pub rules: Vec<String>,
    pub size_bytes: i64,
    pub signer: String,
    pub manifest_hash: String,
    pub uploaded_by: Option<String>,
    pub storage_key: String,
    pub created_ms: i64,
}

impl<'r> FromRow<'r, PgRow> for BundleRow {
    fn from_row(r: &'r PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(BundleRow {
            id: r.try_get("id")?,
            org_id: r.try_get("org_id")?,
            org: r.try_get("org")?,
            repo: r.try_get("repo")?,
            session_id: r.try_get("session_id")?,
            session_name: r.try_get("session_name")?,
            agent: r.try_get("agent")?,
            session_created_ms: r.try_get("session_created_ms")?,
            events: r.try_get("events")?,
            rules: r.try_get("rules")?,
            size_bytes: r.try_get("size_bytes")?,
            signer: r.try_get("signer")?,
            manifest_hash: r.try_get("manifest_hash")?,
            uploaded_by: r.try_get("uploaded_by")?,
            storage_key: r.try_get("storage_key")?,
            created_ms: r.try_get("created_ms")?,
        })
    }
}

impl BundleRow {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "org": self.org,
            "repo": self.repo,
            "session": {
                "id": self.session_id,
                "name": self.session_name,
                "agent": self.agent,
                "created_ms": self.session_created_ms,
            },
            "events": self.events,
            "rules": self.rules,
            "size_bytes": self.size_bytes,
            "signer": self.signer,
            "manifest_hash": self.manifest_hash,
            "uploaded_by": self.uploaded_by,
            "created_ms": self.created_ms,
        })
    }
}

/// The bundle query with a static `WHERE` clause appended.
macro_rules! select {
    ($rest:literal $(,)?) => {
        concat!(
            "SELECT b.id, b.org_id, o.slug AS org, b.repo, b.session_id, b.session_name, \
             b.agent, (extract(epoch FROM b.session_created_at) * 1000)::bigint AS session_created_ms, \
             b.events, b.rules, b.size_bytes, b.signer, b.manifest_hash, b.uploaded_by, b.storage_key, \
             (extract(epoch FROM b.created_at) * 1000)::bigint AS created_ms \
             FROM bundles b JOIN orgs o ON o.id = b.org_id",
            $rest
        )
    };
}
pub(crate) use select;

/// The bundle and the caller's access to its org (404 for strangers).
pub async fn bundle_for(
    st: &AppState,
    caller: &Caller,
    bid: &str,
) -> Result<(BundleRow, OrgAccess), Problem> {
    let pool = st.db()?;
    let row: Option<BundleRow> = db::query_as(select!(" WHERE b.id = $1"))
        .bind(bid)
        .fetch_optional(pool)
        .await?;
    let row = row.ok_or_else(Problem::not_found)?;
    let access = access::org_by_id(pool, caller, &row.org_id).await?;
    Ok((row, access))
}

fn upload_outcome(st: &AppState, outcome: &str) {
    st.metrics
        .inc("wyrd_hub_uploads_total", &[("outcome", outcome)]);
}

fn too_large(st: &AppState, detail: String) -> Problem {
    upload_outcome(st, "too_large");
    Problem::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too-large",
        "Upload too large",
    )
    .detail(detail)
}

/// Read the multipart body: the `bundle` file into `spool`, `repo` text.
async fn receive(
    st: &AppState,
    mp: &mut Multipart,
    spool: &mut Spool,
) -> Result<(u64, Option<String>), Problem> {
    let max = st.config.limits.max_upload_bytes;
    let (mut size, mut repo, mut got) = (0u64, None, false);
    let multipart_err = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            too_large(st, format!("the limit is {max} bytes"))
        } else {
            Problem::bad_request(format!("multipart: {}", e.body_text()))
        }
    };
    while let Some(mut field) = mp.next_field().await.map_err(multipart_err)? {
        match field.name() {
            Some("bundle") if !got => {
                got = true;
                while let Some(chunk) = field.chunk().await.map_err(multipart_err)? {
                    size += chunk.len() as u64;
                    if size > max {
                        return Err(too_large(st, format!("the limit is {max} bytes")));
                    }
                    spool
                        .file
                        .write_all(&chunk)
                        .await
                        .map_err(|e| Problem::internal(&e))?;
                }
            }
            Some("repo") => {
                let text = field.text().await.map_err(multipart_err)?;
                if !ingest::valid_repo(&text) {
                    return Err(Problem::bad_request(
                        "repo: 1-200 characters of letters, digits, and -_./:@",
                    ));
                }
                repo = Some(text);
            }
            other => {
                return Err(Problem::bad_request(format!(
                    "unexpected multipart field {:?}",
                    other.unwrap_or("")
                )));
            }
        }
    }
    if !got {
        return Err(Problem::bad_request("missing the `bundle` file field"));
    }
    spool
        .file
        .flush()
        .await
        .map_err(|e| Problem::internal(&e))?;
    Ok((size, repo))
}

fn storage_problem(e: &StorageError) -> Problem {
    tracing::error!(error = %e, "object storage");
    Problem::unavailable("object storage is unavailable")
}

async fn upload(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
    mut mp: Multipart,
) -> Result<Response, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    a.require(Role::Member)?;
    let mut spool = Spool::create(&st.config.data_dir).await?;
    let (size, repo) = receive(&st, &mut mp, &mut spool).await?;

    let started = Instant::now();
    let verified = ingest::verify(spool.path.clone(), st.config.limits.max_upload_bytes).await;
    st.metrics
        .observe("wyrd_hub_bundle_verify_seconds", &[], started.elapsed());
    let bundle = match verified {
        Ok(b) => b,
        Err(p) => {
            upload_outcome(&st, "rejected");
            return Err(p);
        }
    };
    let summary = ingest::summarize(&bundle);
    #[cfg(feature = "analytics")]
    let rows = crate::projection::rows(&bundle);
    drop(bundle);
    let (id, duplicate) = store(&st, &caller, &a, &spool, size, repo, &summary).await?;
    let (row, _) = bundle_for(&st, &caller, &id).await?;
    let status = if duplicate {
        StatusCode::OK
    } else {
        #[cfg(feature = "analytics")]
        super::analytics::project_upload(&st, &a.org_id, &id, rows).await;
        upload_outcome(&st, "accepted");
        tracing::info!(bundle = id, org = a.slug, size, "bundle uploaded");
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(json!({"duplicate": duplicate, "bundle": row.json()})),
    )
        .into_response())
}

/// Quota check, object upload, row and audit entry (one org at a time).
/// Returns the bundle id and whether the org already had this bundle.
async fn store(
    st: &AppState,
    caller: &Caller,
    a: &OrgAccess,
    spool: &Spool,
    size: u64,
    repo: Option<String>,
    summary: &ingest::Summary,
) -> Result<(String, bool), Problem> {
    let pool = st.db()?;
    let size_i = i64::try_from(size).unwrap_or(i64::MAX);
    let mut tx = pool.begin().await?;
    // One upload per org at a time past this point: quota and dedup.
    let (quota_bytes, quota_bundles): (i64, i64) =
        db::query_as("SELECT quota_bytes, quota_bundles FROM orgs WHERE id = $1 FOR UPDATE")
            .bind(&a.org_id)
            .fetch_one(&mut *tx)
            .await?;
    let existing: Option<String> =
        db::query_scalar("SELECT id FROM bundles WHERE org_id = $1 AND manifest_hash = $2")
            .bind(&a.org_id)
            .bind(&summary.manifest_hash)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(id) = existing {
        upload_outcome(st, "duplicate");
        return Ok((id, true));
    }
    let (count, used): (i64, i64) = db::query_as(
        "SELECT count(*), coalesce(sum(size_bytes), 0)::bigint FROM bundles WHERE org_id = $1",
    )
    .bind(&a.org_id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= quota_bundles {
        upload_outcome(st, "over_quota");
        return Err(Problem::new(
            StatusCode::TOO_MANY_REQUESTS,
            "quota-bundles",
            "Bundle count quota reached",
        )
        .detail(format!("{count} of {quota_bundles} bundles stored")));
    }
    if used.saturating_add(size_i) > quota_bytes {
        upload_outcome(st, "over_quota");
        return Err(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "quota-bytes",
            "Storage quota reached",
        )
        .detail(format!(
            "{used} of {quota_bytes} bytes used, this bundle is {size}"
        )));
    }

    let id = ulid::Ulid::generate().to_string();
    let key = format!("bundles/{}/{id}.wyrd", a.org_id);
    if let Err(e) = st.storage.put_file(&key, spool.path.clone()).await {
        upload_outcome(st, "storage_error");
        return Err(storage_problem(&e));
    }
    let stored = async {
        db::query(
            "INSERT INTO bundles (id, org_id, uploaded_by, manifest_hash, signer, session_id, \
             session_name, agent, session_created_at, events, repo, rules, size_bytes, storage_key) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, to_timestamp($9::bigint / 1000.0), $10, $11, \
             $12, $13, $14)",
        )
        .bind(&id)
        .bind(&a.org_id)
        .bind(&caller.user_id)
        .bind(&summary.manifest_hash)
        .bind(&summary.signer)
        .bind(&summary.session_id)
        .bind(&summary.session_name)
        .bind(&summary.agent)
        .bind(summary.session_created_ms)
        .bind(summary.events)
        .bind(&repo)
        .bind(&summary.rules)
        .bind(size_i)
        .bind(&key)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            Entry {
                org_id: Some(&a.org_id),
                actor: &caller.user_id,
                action: "bundle.upload",
                target: Some(&id),
                request_id: &caller.request_id,
            },
            json!({ "size_bytes": size, "session_id": summary.session_id, "repo": repo }),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = stored {
        // No object without its row.
        if let Err(del) = st.storage.delete(&key).await {
            tracing::error!(error = %del, key, "could not remove the object of a failed upload");
        }
        upload_outcome(st, "error");
        return Err(e.into());
    }
    Ok((id, false))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    repo: Option<String>,
    rule: Option<String>,
    /// RFC 3339 time or date: uploaded at or after.
    since: Option<String>,
    /// Paging: bundles with an id below this one.
    before: Option<String>,
    limit: Option<i64>,
}

fn is_bad_input(e: &sqlx_core::Error) -> bool {
    e.as_database_error()
        .and_then(sqlx_core::error::DatabaseError::code)
        .is_some_and(|c| c.starts_with("22"))
}

async fn list(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows: Vec<BundleRow> = db::query_as(select!(
        " WHERE b.org_id = $1 AND ($2::text IS NULL OR b.repo = $2) \
          AND ($3::text IS NULL OR $3 = ANY (b.rules)) \
          AND ($4::text IS NULL OR b.created_at >= ($4::text)::timestamptz) \
          AND ($5::text IS NULL OR b.id < $5) ORDER BY b.id DESC LIMIT $6",
    ))
    .bind(&a.org_id)
    .bind(&q.repo)
    .bind(&q.rule)
    .bind(&q.since)
    .bind(&q.before)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        if is_bad_input(&e) {
            Problem::bad_request("since: expected an RFC 3339 time or a date")
        } else {
            e.into()
        }
    })?;
    Ok(Json(
        json!({ "bundles": rows.iter().map(BundleRow::json).collect::<Vec<_>>() }),
    ))
}

async fn show(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(bid): Path<String>,
) -> Result<Json<Value>, Problem> {
    let (row, _) = bundle_for(&st, &caller, &bid).await?;
    Ok(Json(row.json()))
}

async fn download(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(bid): Path<String>,
) -> Result<Response, Problem> {
    let (row, _) = bundle_for(&st, &caller, &bid).await?;
    let obj = st
        .storage
        .get(&row.storage_key)
        .await
        .map_err(|e| storage_problem(&e))?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    let mut reader = obj.reader;
    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let name: String = row
        .session_name
        .as_deref()
        .unwrap_or(&row.session_id)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut res = Response::new(Body::from_stream(stream));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(v) = header::HeaderValue::from_str(&format!("attachment; filename=\"{name}.wyrd\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(len) = obj.len {
        h.insert(header::CONTENT_LENGTH, header::HeaderValue::from(len));
    }
    Ok(res)
}

/// Delete a bundle's row and its object (admins).
async fn remove(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(bid): Path<String>,
) -> Result<StatusCode, Problem> {
    let (row, a) = bundle_for(&st, &caller, &bid).await?;
    a.require(Role::Admin)?;
    delete_bundle(
        &st,
        &row,
        &caller.user_id,
        &caller.request_id,
        "bundle.delete",
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove the row (with its audit entry), then the object. A failed
/// object delete leaves an unreferenced object, never a dangling row.
pub async fn delete_bundle(
    st: &AppState,
    row: &BundleRow,
    actor: &str,
    request_id: &str,
    action: &str,
) -> Result<(), Problem> {
    let pool = st.db()?;
    let mut tx = pool.begin().await?;
    let gone = db::query("DELETE FROM bundles WHERE id = $1")
        .bind(&row.id)
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Problem::not_found());
    }
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&row.org_id),
            actor,
            action,
            target: Some(&row.id),
            request_id,
        },
        json!({ "session_id": row.session_id, "size_bytes": row.size_bytes }),
    )
    .await?;
    tx.commit().await?;
    if let Err(e) = st.storage.delete(&row.storage_key).await {
        tracing::error!(error = %e, key = row.storage_key, "bundle object not deleted");
    }
    Ok(())
}

/// `PATCH /api/v1/orgs/{org}`: `retention_days` (1-3650, or null to keep
/// bundles forever). Admins only.
async fn update_org(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    a.require(Role::Admin)?;
    let obj = body
        .as_object()
        .ok_or_else(|| Problem::bad_request("expected a JSON object"))?;
    if let Some(k) = obj.keys().find(|k| *k != "retention_days") {
        return Err(Problem::bad_request(format!("unknown field {k:?}")));
    }
    let Some(v) = obj.get("retention_days") else {
        return Err(Problem::bad_request("nothing to change"));
    };
    let days = match v {
        Value::Null => None,
        v => Some(
            v.as_i64()
                .filter(|d| (1..=3650).contains(d))
                .and_then(|d| i32::try_from(d).ok())
                .ok_or_else(|| Problem::bad_request("retention_days: 1-3650 or null"))?,
        ),
    };
    let mut tx = pool.begin().await?;
    db::query("UPDATE orgs SET retention_days = $2 WHERE id = $1")
        .bind(&a.org_id)
        .bind(days)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&a.org_id),
            actor: &caller.user_id,
            action: "org.update",
            target: Some(&a.slug),
            request_id: &caller.request_id,
        },
        json!({ "retention_days": days }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "slug": a.slug, "retention_days": days })))
}
