//! CI uploads without a stored secret: a GitHub Actions workflow exchanges
//! its OIDC token for a short-lived upload-only token of the organisation
//! that trusts its repository (`docs/architecture/hub.md`, "CI upload
//! tokens"). Admins manage the trust rules.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::{self, Entry};
use crate::auth::{self, CI_TOKEN_PREFIX, Caller, Role};
use crate::db;
use crate::jwt::{self, CiIdentity};
use crate::{AppState, Problem};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/ci/token", post(exchange))
        .route(
            "/api/v1/orgs/{org}/ci-trust",
            get(list_rules).post(add_rule),
        )
        .route("/api/v1/orgs/{org}/ci-trust/{rule}", delete(remove_rule))
}

/// Organisation slugs, as the schema constrains them.
fn valid_slug(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && s.as_bytes()[0] != b'-'
}

/// A GitHub login: 1–39 alphanumerics or hyphens, no leading hyphen.
fn valid_owner(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        && s.as_bytes()[0] != b'-'
}

/// `owner/name` with a GitHub repository name (1–100 of `[A-Za-z0-9._-]`).
fn valid_repository(s: &str) -> bool {
    s.split_once('/').is_some_and(|(owner, name)| {
        valid_owner(owner)
            && (1..=100).contains(&name.len())
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    })
}

/// A git ref a rule may pin to: `refs/…`, printable, at most 255 bytes, and
/// no glob (a pin is exact).
fn valid_git_ref(s: &str) -> bool {
    s.starts_with("refs/")
        && s.len() <= 255
        && s.bytes()
            .all(|c| c.is_ascii_graphic() && !matches!(c, b'*' | b'?' | b'['))
}

/// The organisation's id when `caller` is one of its admins; 404 for
/// a stranger (or a token of another organisation), 403 for a member.
async fn org_admin(st: &AppState, caller: &Caller, slug: &str) -> Result<String, Problem> {
    if !valid_slug(slug) {
        return Err(Problem::not_found());
    }
    let pool = st.db()?;
    let row: Option<(String, Option<String>)> = db::query_as(
        "SELECT o.id, m.role FROM orgs o \
         LEFT JOIN memberships m ON m.org_id = o.id AND m.user_id = $2 \
         WHERE o.slug = $1",
    )
    .bind(slug)
    .bind(&caller.user_id)
    .fetch_optional(pool)
    .await?;
    let (org_id, role) = row.ok_or_else(Problem::not_found)?;
    if caller.token_org.as_ref().is_some_and(|t| *t != org_id) {
        return Err(Problem::not_found());
    }
    match role.as_deref().and_then(Role::parse) {
        None => Err(Problem::not_found()),
        Some(r) if r < Role::Admin => Err(Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Admin role required",
        )),
        Some(_) => Ok(org_id),
    }
}

/// id, owner, repository, wildcard, enabled, `git_ref`, `created_by`, `created_at`.
type RuleRow = (
    String,
    String,
    Option<String>,
    bool,
    bool,
    Option<String>,
    Option<String>,
    String,
);

fn rule_json(
    (id, owner, repository, wildcard, enabled, git_ref, created_by, created_at): RuleRow,
) -> Value {
    json!({
        "id": id,
        "repository_owner": owner,
        "repository": repository,
        "wildcard": wildcard,
        "enabled": enabled,
        "git_ref": git_ref,
        "created_by": created_by,
        "created_at": created_at,
    })
}

async fn list_rules(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(org): Path<String>,
) -> Result<Json<Value>, Problem> {
    let org_id = org_admin(&st, &caller, &org).await?;
    let rows: Vec<RuleRow> = db::query_as(
        "SELECT id, repository_owner, repository, wildcard, enabled, git_ref, created_by, \
                to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
         FROM ci_trust_rules WHERE org_id = $1 \
         ORDER BY repository_owner, repository NULLS FIRST",
    )
    .bind(&org_id)
    .fetch_all(st.db()?)
    .await?;
    Ok(Json(json!({
        "rules": rows.into_iter().map(rule_json).collect::<Vec<_>>(),
    })))
}

/// A new trust rule. Without `repository` the rule is owner-wide, and that
/// needs `wildcard: true`: every repository of the owner may then upload.
#[derive(Debug, Deserialize)]
struct NewRule {
    repository_owner: String,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    wildcard: bool,
    /// Only runs of this git ref (`refs/heads/main`, `refs/tags/v1.2.3`).
    #[serde(default)]
    git_ref: Option<String>,
}

/// The scope of a new rule: exact, or owner-wide only with the opt-in, and a
/// git ref pin that is a full, glob-free ref.
fn check_scope(
    repository: Option<&str>,
    wildcard: bool,
    git_ref: Option<&str>,
) -> Result<(), Problem> {
    match (repository, wildcard) {
        (None, false) => {
            return Err(Problem::bad_request(
                "an owner-wide rule needs \"wildcard\": true: every repository of the owner \
                 could then upload to this organisation",
            ));
        }
        (Some(_), true) => {
            return Err(Problem::bad_request(
                "\"wildcard\" is for owner-wide rules: leave repository out",
            ));
        }
        _ => {}
    }
    if git_ref.is_some_and(|r| !valid_git_ref(r)) {
        return Err(Problem::bad_request(
            "git_ref must be a full ref such as refs/heads/main (no wildcards)",
        ));
    }
    Ok(())
}

/// The 409 for a rule that is already there. A disabled rule (a migrated
/// owner-wide one) blocks the re-add: the admin deletes it first.
async fn rule_exists(
    tx: &mut sqlx_core::transaction::Transaction<'_, db::Postgres>,
    org_id: &str,
    owner: &str,
    repository: Option<&str>,
) -> Result<Problem, Problem> {
    let disabled: Option<bool> = db::query_scalar(
        "SELECT NOT enabled FROM ci_trust_rules WHERE org_id = $1 AND repository_owner = $2 \
           AND repository IS NOT DISTINCT FROM $3",
    )
    .bind(org_id)
    .bind(owner)
    .bind(repository)
    .fetch_optional(&mut **tx)
    .await?;
    let mut problem = Problem::new(
        StatusCode::CONFLICT,
        "exists",
        "This trust rule already exists",
    );
    if disabled == Some(true) {
        problem.detail =
            Some("A disabled rule for this owner exists: delete it before adding it again".into());
    }
    Ok(problem)
}

async fn add_rule(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path(org): Path<String>,
    Json(rule): Json<NewRule>,
) -> Result<(StatusCode, Json<Value>), Problem> {
    let org_id = org_admin(&st, &caller, &org).await?;
    let owner = rule.repository_owner.trim();
    if !valid_owner(owner) {
        return Err(Problem::bad_request(
            "repository_owner must be a GitHub user or organisation login",
        ));
    }
    let repository = rule
        .repository
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    if let Some(r) = repository {
        if !valid_repository(r) {
            return Err(Problem::bad_request("repository must be owner/name"));
        }
        if !r
            .split_once('/')
            .is_some_and(|(o, _)| o.eq_ignore_ascii_case(owner))
        {
            return Err(Problem::bad_request(
                "repository must belong to repository_owner",
            ));
        }
    }
    let git_ref = rule
        .git_ref
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    check_scope(repository, rule.wildcard, git_ref)?;
    let wildcard = rule.wildcard;
    let pool = st.db()?;
    let mut tx = pool.begin().await?;
    let id = ulid::Ulid::generate().to_string();
    let row: Option<RuleRow> = db::query_as(
        "INSERT INTO ci_trust_rules \
             (id, org_id, repository_owner, repository, wildcard, git_ref, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (org_id, repository_owner, repository) DO NOTHING \
         RETURNING id, repository_owner, repository, wildcard, enabled, git_ref, created_by, \
                   to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .bind(&id)
    .bind(&org_id)
    .bind(owner)
    .bind(repository)
    .bind(wildcard)
    .bind(git_ref)
    .bind(&caller.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Err(rule_exists(&mut tx, &org_id, owner, repository).await?);
    };
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&org_id),
            actor: &caller.user_id,
            action: "ci.trust.add",
            target: Some(&id),
            request_id: &caller.request_id,
        },
        json!({
            "repository_owner": owner,
            "repository": repository,
            "wildcard": wildcard,
            "git_ref": git_ref,
        }),
    )
    .await?;
    if wildcard {
        // The dangerous opt-in gets its own audit action, so it is easy to find.
        audit::record(
            &mut *tx,
            Entry {
                org_id: Some(&org_id),
                actor: &caller.user_id,
                action: "ci.trust.wildcard",
                target: Some(&id),
                request_id: &caller.request_id,
            },
            json!({
                "repository_owner": owner,
                "dangerous": true,
                "note": "every repository of the owner may upload to this organisation",
            }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(rule_json(row))))
}

async fn remove_rule(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Path((org, rule)): Path<(String, String)>,
) -> Result<StatusCode, Problem> {
    let org_id = org_admin(&st, &caller, &org).await?;
    let pool = st.db()?;
    let mut tx = pool.begin().await?;
    let removed: Option<(String, Option<String>)> = db::query_as(
        "DELETE FROM ci_trust_rules WHERE id = $1 AND org_id = $2 \
         RETURNING repository_owner, repository",
    )
    .bind(&rule)
    .bind(&org_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((owner, repository)) = removed else {
        return Err(Problem::not_found());
    };
    // CI identities (subject `repo:owner/name`) no remaining rule covers
    // lose their membership, and their live upload tokens with it.
    db::query(
        "DELETE FROM memberships m USING users u \
         WHERE m.org_id = $1 AND m.user_id = u.id AND u.issuer = $2 \
           AND u.subject LIKE 'repo:%' \
           AND NOT EXISTS (\
             SELECT 1 FROM ci_trust_rules r WHERE r.org_id = m.org_id AND r.enabled \
               AND lower(r.repository_owner) = split_part(substr(u.subject, 6), '/', 1) \
               AND (r.wildcard OR lower(r.repository) = substr(u.subject, 6)))",
    )
    .bind(&org_id)
    .bind(st.ci.issuer())
    .execute(&mut *tx)
    .await?;
    db::query(
        "DELETE FROM api_tokens t WHERE t.org_id = $1 AND t.scope = 'upload' \
         AND NOT EXISTS (SELECT 1 FROM memberships m \
                         WHERE m.org_id = t.org_id AND m.user_id = t.user_id)",
    )
    .bind(&org_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(&org_id),
            actor: &caller.user_id,
            action: "ci.trust.remove",
            target: Some(&rule),
            request_id: &caller.request_id,
        },
        json!({ "repository_owner": owner, "repository": repository }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ExchangeRequest {
    /// The GitHub Actions OIDC token (`ACTIONS_ID_TOKEN_REQUEST_URL`).
    token: String,
    /// The organisation to upload to.
    org: String,
}

/// A refused exchange: counted, logged with its reason, answered without it.
fn rejected(st: &AppState, status: StatusCode, kind: &str, reason: &str) -> Problem {
    st.metrics.inc(
        "wyrd_hub_ci_token_exchanges_total",
        &[("outcome", "failure")],
    );
    tracing::warn!(reason, "ci token exchange refused");
    Problem::new(status, kind, "CI token refused")
}

/// The subject of the user row a repository's workflows act as.
fn ci_subject(repository: &str) -> String {
    format!("repo:{}", repository.to_ascii_lowercase())
}

async fn exchange(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ExchangeRequest>,
) -> Result<Json<Value>, Problem> {
    if !valid_slug(&req.org) {
        return Err(Problem::bad_request("org must be an organisation slug"));
    }
    // The token is verified before the database is touched, so a bad
    // token costs nothing but a cached key lookup.
    let claims = match st.ci.verify(&req.token).await {
        Ok(c) => c,
        Err(e) if e.is_unavailable() => {
            st.metrics.inc(
                "wyrd_hub_ci_token_exchanges_total",
                &[("outcome", "failure")],
            );
            tracing::warn!(error = %e, "ci token issuer");
            return Err(Problem::unavailable(
                "the token issuer is unavailable; retry later",
            ));
        }
        Err(e) => {
            return Err(rejected(
                &st,
                StatusCode::UNAUTHORIZED,
                "ci-token-rejected",
                &e.to_string(),
            ));
        }
    };
    let id = jwt::check_ci_token(
        &claims,
        &st.config.ci.issuer,
        &st.config.ci.audience,
        jwt::now(),
    )
    .map_err(|e| {
        rejected(
            &st,
            StatusCode::UNAUTHORIZED,
            "ci-token-rejected",
            &e.to_string(),
        )
    })?;
    let pool = st.db()?;
    // An enabled rule must name the repository exactly, or be an owner-wide
    // opt-in (`wildcard`) of its owner; a rule pinned to a git ref also needs
    // the run to be on that ref. Exact rules win over owner-wide ones.
    let row: Option<(String, Option<String>, Option<bool>)> = db::query_as(
        "SELECT o.id, r.id, r.wildcard FROM orgs o \
         LEFT JOIN LATERAL (\
            SELECT r.id, r.wildcard FROM ci_trust_rules r \
            WHERE r.org_id = o.id AND r.enabled \
              AND lower(r.repository_owner) = lower($2) \
              AND (r.wildcard OR lower(r.repository) = lower($3)) \
              AND (r.git_ref IS NULL OR r.git_ref = $4) \
            ORDER BY r.wildcard, (r.git_ref IS NULL) LIMIT 1) r ON true \
         WHERE o.slug = $1",
    )
    .bind(&req.org)
    .bind(&id.repository_owner)
    .bind(&id.repository)
    .bind(id.git_ref.as_deref())
    .fetch_optional(pool)
    .await?;
    let Some((org_id, Some(rule_id), Some(wildcard))) = row else {
        return Err(rejected(
            &st,
            StatusCode::FORBIDDEN,
            "ci-not-trusted",
            &format!("no trust rule for {} in {:?}", id.repository, req.org),
        ));
    };
    let (token, token_id, expires_at) =
        mint(&st, pool, &headers, &org_id, &rule_id, wildcard, &id).await?;
    st.metrics.inc(
        "wyrd_hub_ci_token_exchanges_total",
        &[("outcome", "success")],
    );
    tracing::info!(
        repository = id.repository,
        org = req.org,
        token_id,
        "ci token exchanged"
    );
    Ok(Json(json!({
        "token": token,
        "token_type": "Bearer",
        "scope": "upload",
        "org": req.org,
        "expires_in": st.config.ci.token_ttl_secs,
        "expires_at": expires_at,
    })))
}

/// The CI user, its membership, the token row, and the audit row, in one
/// transaction. Returns the secret, the token id, and the expiry.
async fn mint(
    st: &AppState,
    pool: &db::PgPool,
    headers: &HeaderMap,
    org_id: &str,
    rule_id: &str,
    wildcard: bool,
    id: &CiIdentity,
) -> Result<(String, String, String), Problem> {
    let mut tx = pool.begin().await?;
    let user_id: String = db::query_scalar(
        "INSERT INTO users (id, issuer, subject, name) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (issuer, subject) DO UPDATE SET name = EXCLUDED.name \
         RETURNING id",
    )
    .bind(ulid::Ulid::generate().to_string())
    .bind(st.ci.issuer())
    .bind(ci_subject(&id.repository))
    .bind(format!("{} (GitHub Actions)", id.repository))
    .fetch_one(&mut *tx)
    .await?;
    db::query(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, 'member') \
         ON CONFLICT (org_id, user_id) DO NOTHING",
    )
    .bind(org_id)
    .bind(&user_id)
    .execute(&mut *tx)
    .await?;
    db::query("DELETE FROM api_tokens WHERE scope = 'upload' AND expires_at < now()")
        .execute(&mut *tx)
        .await?;
    let token = auth::random_secret(CI_TOKEN_PREFIX)?;
    let token_id = ulid::Ulid::generate().to_string();
    let name = format!(
        "github-actions {} run {}",
        id.repository,
        id.run_id.as_deref().unwrap_or("?")
    );
    let expires_at: String = db::query_scalar(
        "INSERT INTO api_tokens (id, token_hash, user_id, org_id, name, scope, expires_at) \
         VALUES ($1, $2, $3, $4, left($5, 100), 'upload', now() + make_interval(secs => $6::bigint)) \
         RETURNING to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .bind(&token_id)
    .bind(auth::hash(&token))
    .bind(&user_id)
    .bind(org_id)
    .bind(&name)
    .bind(st.config.ci.token_ttl_secs)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: Some(org_id),
            actor: &user_id,
            action: "ci.token.exchange",
            target: Some(&token_id),
            request_id: &auth::request_id(headers),
        },
        json!({
            "rule": rule_id,
            "wildcard": wildcard,
            "repository": id.repository,
            "repository_owner": id.repository_owner,
            "repository_id": id.repository_id,
            "repository_owner_id": id.repository_owner_id,
            "actor": id.actor,
            "workflow_ref": id.workflow_ref,
            "run_id": id.run_id,
            "ref": id.git_ref,
            "sha": id.sha,
            "event_name": id.event_name,
            "expires_at": expires_at,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok((token, token_id, expires_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_refs_are_exact() {
        for ok in [
            "refs/heads/main",
            "refs/tags/v1.2.3",
            "refs/heads/feature/x",
        ] {
            assert!(valid_git_ref(ok), "{ok}");
        }
        for bad in [
            "main",
            "refs/heads/*",
            "refs/heads/a?b",
            "refs/heads/a b",
            "",
        ] {
            assert!(!valid_git_ref(bad), "{bad:?}");
        }
        assert!(!valid_git_ref(&format!("refs/{}", "x".repeat(260))));
    }

    #[test]
    fn github_names() {
        for ok in ["acme", "octo-cat", "a", "x1"] {
            assert!(valid_owner(ok), "{ok}");
        }
        for bad in ["", "-acme", "acme/app", "a b", &"x".repeat(40)] {
            assert!(!valid_owner(bad), "{bad:?}");
        }
        for ok in ["acme/app", "acme/my.repo_v2", "Acme/App-1"] {
            assert!(valid_repository(ok), "{ok}");
        }
        for bad in [
            "acme",
            "acme/",
            "/app",
            "acme/a b",
            "acme/app/extra",
            "-x/app",
        ] {
            assert!(!valid_repository(bad), "{bad:?}");
        }
        assert!(valid_slug("acme-1"));
        assert!(!valid_slug("Acme") && !valid_slug("-a") && !valid_slug(""));
        assert_eq!(ci_subject("Acme/App"), "repo:acme/app");
    }
}
