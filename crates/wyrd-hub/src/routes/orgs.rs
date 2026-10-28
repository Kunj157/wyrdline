//! Organisations, memberships, API tokens, and the audit log.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{self, OrgAccess};
use crate::audit::{self, Entry};
use crate::auth::{self, Caller, Role, Via};
use crate::db;
use crate::{AppState, Problem};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/orgs", post(create_org))
        .route("/api/v1/orgs/{org}", get(get_org))
        .route("/api/v1/orgs/{org}/members", get(list_members))
        .route(
            "/api/v1/orgs/{org}/members/{user}",
            put(set_member).delete(remove_member),
        )
        .route("/api/v1/tokens", post(create_token).get(list_tokens))
        .route("/api/v1/tokens/{id}", delete(revoke_token))
        .route("/api/v1/audit-logs", get(audit_logs))
}

fn bad(detail: &str) -> Problem {
    Problem::bad_request(detail)
}

fn conflict(detail: &str) -> Problem {
    Problem::new(StatusCode::CONFLICT, "conflict", "Conflict").detail(detail)
}

fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=63).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

#[derive(Debug, Deserialize)]
struct NewOrg {
    slug: String,
    name: String,
}

/// Any signed-in user may create an organisation and becomes its admin.
async fn create_org(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Json(body): Json<NewOrg>,
) -> Result<Response, Problem> {
    if caller.via == Via::Token && caller.token_org.is_some() {
        return Err(Problem::not_found());
    }
    if !valid_slug(&body.slug) {
        return Err(bad("slug: 1-63 lowercase letters, digits, or hyphens"));
    }
    let name = body.name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(bad("name: 1-200 characters"));
    }
    let pool = st.db()?;
    let mut tx = pool.begin().await?;
    let id = ulid::Ulid::generate().to_string();
    let inserted = db::query(
        "INSERT INTO orgs (id, slug, name, quota_bytes, quota_bundles) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (slug) DO NOTHING",
    )
    .bind(&id)
    .bind(&body.slug)
    .bind(name)
    .bind(st.config.limits.quota_bytes)
    .bind(st.config.limits.quota_bundles)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(conflict("an organisation with this slug exists"));
    }
    db::query("INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, 'admin')")
        .bind(&id)
        .bind(&caller.user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&id),
            actor: &caller.user_id,
            action: "org.create",
            target: Some(&body.slug),
            request_id: &caller.request_id,
        },
        json!({ "name": name }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "slug": body.slug, "name": name, "role": "admin" })),
    )
        .into_response())
}

async fn get_org(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    let (name, quota_bytes, quota_bundles, retention_days): (String, i64, i64, Option<i32>) =
        db::query_as(
            "SELECT name, quota_bytes, quota_bundles, retention_days FROM orgs WHERE id = $1",
        )
        .bind(&a.org_id)
        .fetch_one(pool)
        .await?;
    Ok(Json(json!({
        "id": a.org_id, "slug": a.slug, "name": name, "role": a.role,
        "quota_bytes": quota_bytes, "quota_bundles": quota_bundles,
        "retention_days": retention_days,
    })))
}

async fn list_members(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(slug): Path<String>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    let rows: Vec<(String, Option<String>, Option<String>, String)> = db::query_as(
        "SELECT u.id, u.email, u.name, m.role FROM memberships m JOIN users u ON u.id = m.user_id \
         WHERE m.org_id = $1 ORDER BY u.email NULLS LAST, u.id",
    )
    .bind(&a.org_id)
    .fetch_all(pool)
    .await?;
    let members: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, email, name, role)| json!({"id": id, "email": email, "name": name, "role": role}),
        )
        .collect();
    Ok(Json(json!({ "members": members })))
}

#[derive(Debug, Deserialize)]
struct SetRole {
    role: String,
}

/// The user `user` names: a user id, or an email address (contains `@`).
async fn find_user(pool: &db::PgPool, user: &str) -> Result<String, Problem> {
    let sql = if user.contains('@') {
        "SELECT id FROM users WHERE lower(email) = lower($1) ORDER BY created_at LIMIT 1"
    } else {
        "SELECT id FROM users WHERE id = $1"
    };
    db::query_scalar(sql)
        .bind(user)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| Problem::not_found().detail("no such user (they must sign in once first)"))
}

/// Refuse to leave an org without an admin.
async fn keeps_an_admin(
    tx: &mut sqlx_core::transaction::Transaction<'_, db::Postgres>,
    a: &OrgAccess,
    user_id: &str,
) -> Result<(), Problem> {
    let others: i64 = db::query_scalar(
        "SELECT count(*) FROM memberships WHERE org_id = $1 AND role = 'admin' AND user_id <> $2",
    )
    .bind(&a.org_id)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    if others == 0 {
        Err(conflict("an organisation needs at least one admin"))
    } else {
        Ok(())
    }
}

async fn set_member(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path((slug, user)): Path<(String, String)>,
    Json(body): Json<SetRole>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    a.require(Role::Admin)?;
    let role = Role::parse(&body.role).ok_or_else(|| bad("role: viewer, member, or admin"))?;
    let user_id = find_user(pool, &user).await?;
    let mut tx = pool.begin().await?;
    // Serialise membership changes of one org (the last-admin check).
    db::query("SELECT id FROM orgs WHERE id = $1 FOR UPDATE")
        .bind(&a.org_id)
        .execute(&mut *tx)
        .await?;
    if role != Role::Admin {
        keeps_an_admin(&mut tx, &a, &user_id).await?;
    }
    let previous: Option<String> =
        db::query_scalar("SELECT role FROM memberships WHERE org_id = $1 AND user_id = $2")
            .bind(&a.org_id)
            .bind(&user_id)
            .fetch_optional(&mut *tx)
            .await?;
    db::query(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3) \
         ON CONFLICT (org_id, user_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(&a.org_id)
    .bind(&user_id)
    .bind(role.as_str())
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&a.org_id),
            actor: &caller.user_id,
            action: "org.member.set",
            target: Some(&user_id),
            request_id: &caller.request_id,
        },
        json!({ "role": role, "previous": previous }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "user_id": user_id, "role": role })))
}

async fn remove_member(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path((slug, user)): Path<(String, String)>,
) -> Result<StatusCode, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &slug).await?;
    a.require(Role::Admin)?;
    let user_id = find_user(pool, &user).await?;
    let mut tx = pool.begin().await?;
    db::query("SELECT id FROM orgs WHERE id = $1 FOR UPDATE")
        .bind(&a.org_id)
        .execute(&mut *tx)
        .await?;
    keeps_an_admin(&mut tx, &a, &user_id).await?;
    let removed = db::query("DELETE FROM memberships WHERE org_id = $1 AND user_id = $2")
        .bind(&a.org_id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(Problem::not_found());
    }
    // Tokens restricted to this org stop working with the membership.
    db::query("DELETE FROM api_tokens WHERE org_id = $1 AND user_id = $2")
        .bind(&a.org_id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&a.org_id),
            actor: &caller.user_id,
            action: "org.member.remove",
            target: Some(&user_id),
            request_id: &caller.request_id,
        },
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// id, name, org slug, expiry and last use (ms).
type TokenRow = (String, String, Option<String>, Option<i64>, Option<i64>);
/// id, actor, action, target, request id, details, time (ms).
type AuditRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    sqlx_core::types::Json<Value>,
    i64,
);

#[derive(Debug, Deserialize)]
struct NewToken {
    name: String,
    /// Restrict the token to this organisation (slug).
    org: Option<String>,
    /// Days until it expires (default 90, at most 365).
    expires_in_days: Option<u16>,
}

/// API tokens are created from a browser session only (a token cannot
/// mint tokens). The secret is returned once.
async fn create_token(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Json(body): Json<NewToken>,
) -> Result<Response, Problem> {
    if caller.via != Via::Session {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "API tokens are created from a signed-in session",
        ));
    }
    let name = body.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(bad("name: 1-100 characters"));
    }
    let days = body.expires_in_days.unwrap_or(90);
    if !(1..=365).contains(&days) {
        return Err(bad("expires_in_days: 1-365"));
    }
    let pool = st.db()?;
    let org_id = match &body.org {
        Some(slug) => Some(access::org(pool, &caller, slug).await?.org_id),
        None => None,
    };
    let secret = auth::random_secret(auth::TOKEN_PREFIX)?;
    let id = ulid::Ulid::generate().to_string();
    let mut tx = pool.begin().await?;
    db::query(
        "INSERT INTO api_tokens (id, token_hash, user_id, org_id, name, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(days => $6::int))",
    )
    .bind(&id)
    .bind(auth::hash(&secret))
    .bind(&caller.user_id)
    .bind(&org_id)
    .bind(name)
    .bind(i32::from(days))
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: org_id.as_deref(),
            actor: &caller.user_id,
            action: "token.create",
            target: Some(&id),
            request_id: &caller.request_id,
        },
        json!({ "name": name, "expires_in_days": days }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "name": name, "org": body.org, "token": secret })),
    )
        .into_response())
}

async fn list_tokens(
    State(st): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let rows: Vec<TokenRow> = db::query_as(
        "SELECT t.id, t.name, o.slug, \
                (extract(epoch FROM t.expires_at) * 1000)::bigint, \
                (extract(epoch FROM t.last_used_at) * 1000)::bigint \
         FROM api_tokens t LEFT JOIN orgs o ON o.id = t.org_id \
         WHERE t.user_id = $1 ORDER BY t.created_at DESC",
    )
    .bind(&caller.user_id)
    .fetch_all(pool)
    .await?;
    let tokens: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, org, exp, used)| {
            json!({"id": id, "name": name, "org": org, "expires_ms": exp, "last_used_ms": used})
        })
        .collect();
    Ok(Json(json!({ "tokens": tokens })))
}

async fn revoke_token(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<StatusCode, Problem> {
    let pool = st.db()?;
    let mut tx = pool.begin().await?;
    let org: Option<Option<String>> =
        db::query_scalar("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2 RETURNING org_id")
            .bind(&id)
            .bind(&caller.user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(org_id) = org else {
        return Err(Problem::not_found());
    };
    audit::record(
        &mut *tx,
        Entry {
            org_id: org_id.as_deref(),
            actor: &caller.user_id,
            action: "token.revoke",
            target: Some(&id),
            request_id: &caller.request_id,
        },
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    org: String,
    limit: Option<i64>,
    /// Entries with an id below this (paging, newest first).
    before: Option<i64>,
}

/// The audit log of one organisation, newest first (admins only).
async fn audit_logs(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, Problem> {
    let pool = st.db()?;
    let a = access::org(pool, &caller, &q.org).await?;
    a.require(Role::Admin)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<AuditRow> = db::query_as(
        "SELECT id, actor, action, target, request_id, details, \
                    (extract(epoch FROM created_at) * 1000)::bigint \
             FROM audit_logs WHERE org_id = $1 AND ($2::bigint IS NULL OR id < $2) \
             ORDER BY id DESC LIMIT $3",
    )
    .bind(&a.org_id)
    .bind(q.before)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(id, actor, action, target, request_id, details, ms)| {
            json!({
                "id": id, "actor": actor, "action": action, "target": target,
                "request_id": request_id, "details": details.0, "time_ms": ms,
            })
        })
        .collect();
    Ok(Json(json!({ "entries": entries })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in ["acme", "a", "shop-2", "0x"] {
            assert!(valid_slug(ok), "{ok}");
        }
        for bad in ["", "-x", "Acme", "a_b", "a b", &"x".repeat(64)] {
            assert!(!valid_slug(bad), "{bad}");
        }
    }
}
