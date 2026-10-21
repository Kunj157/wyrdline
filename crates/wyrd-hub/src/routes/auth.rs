//! OIDC login (authorization code + PKCE, state, nonce), logout, and the
//! signed-in user.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::audit::{self, Entry};
use crate::auth::{self, Caller, Via};
use crate::db;
use crate::oidc::{self, OidcError, Provider};
use crate::{AppState, Problem};

/// How long a started login stays valid.
const LOGIN_SECS: i64 = 600;
const CALLBACK: &str = "/api/v1/auth/oidc/callback";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/auth/oidc/login", get(login))
        .route(CALLBACK, get(callback_get).post(callback_post))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/me", get(me))
}

fn provider(st: &AppState) -> Result<&Provider, Problem> {
    st.oidc
        .as_ref()
        .ok_or_else(|| Problem::not_found().detail("login is not configured"))
}

fn redirect_uri(st: &AppState) -> String {
    format!("{}{CALLBACK}", st.config.public_url)
}

/// A local path to return to after login; anything else becomes `/`.
fn safe_return_to(p: Option<&str>) -> String {
    match p {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && p.len() <= 512
                && !p.contains(['\\', '\r', '\n']) =>
        {
            p.to_string()
        }
        _ => "/".to_string(),
    }
}

fn idp_problem(e: &OidcError) -> Problem {
    tracing::warn!(error = %e, "identity provider");
    Problem::unavailable("the identity provider is unavailable")
}

#[derive(Debug, Deserialize)]
struct LoginQuery {
    return_to: Option<String>,
}

async fn login(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LoginQuery>,
) -> Result<Response, Problem> {
    let provider = provider(&st)?;
    let pool = st.db()?;
    let state = auth::random_secret("")?;
    let nonce = auth::random_secret("")?;
    let verifier = auth::random_secret("")?;
    let binding = auth::random_secret("")?;
    let url = provider
        .authorize_url(
            &redirect_uri(&st),
            &state,
            &nonce,
            &oidc::challenge(&verifier),
        )
        .await
        .map_err(|e| idp_problem(&e))?;
    db::query("DELETE FROM oidc_logins WHERE expires_at < now()")
        .execute(pool)
        .await?;
    db::query(
        "INSERT INTO oidc_logins (state_hash, browser_hash, nonce, pkce_verifier, return_to, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6::bigint))",
    )
    .bind(auth::hash(&state))
    .bind(auth::hash(&binding))
    .bind(&nonce)
    .bind(&verifier)
    .bind(safe_return_to(q.return_to.as_deref()))
    .bind(LOGIN_SECS)
    .execute(pool)
    .await?;
    let cookie = auth::set_cookie(&st, auth::login_cookie_name(&st), &binding, LOGIN_SECS);
    Ok((
        StatusCode::SEE_OTHER,
        [(header::LOCATION, url)],
        [(header::SET_COOKIE, cookie)],
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// A failed login: counted, logged with its reason, and answered 401
/// without detail.
fn failed(st: &AppState, reason: &str) -> Problem {
    st.metrics
        .inc("wyrd_hub_logins_total", &[("outcome", "failure")]);
    tracing::warn!(reason, "login failed");
    Problem::new(StatusCode::UNAUTHORIZED, "login-failed", "Login failed")
}

/// Finish a login; returns the user id, the session secret, and where to go.
async fn finish(
    st: &AppState,
    headers: &HeaderMap,
    p: CallbackParams,
) -> Result<(String, String, String), Problem> {
    let provider = provider(st)?;
    let pool = st.db()?;
    if let Some(e) = p.error {
        return Err(failed(st, &format!("provider error {e:?}")));
    }
    let (Some(code), Some(state)) = (p.code, p.state) else {
        return Err(failed(st, "missing code or state"));
    };
    let Some(binding) = auth::cookie(headers, auth::login_cookie_name(st)) else {
        return Err(failed(st, "no login cookie"));
    };
    // One use only: the row is gone whatever happens next.
    let row: Option<(String, String, String, String, bool)> = db::query_as(
        "DELETE FROM oidc_logins WHERE state_hash = $1 \
         RETURNING browser_hash, nonce, pkce_verifier, return_to, expires_at > now()",
    )
    .bind(auth::hash(&state))
    .fetch_optional(pool)
    .await?;
    let Some((browser_hash, nonce, verifier, return_to, live)) = row else {
        return Err(failed(st, "unknown state"));
    };
    if browser_hash != auth::hash(binding) {
        return Err(failed(st, "login started in another browser"));
    }
    if !live {
        return Err(failed(st, "login expired"));
    }
    let id_token = match provider.exchange(&code, &redirect_uri(st), &verifier).await {
        Ok(t) => t,
        Err(e @ (OidcError::Unreachable(_) | OidcError::Status(500..))) => {
            return Err(idp_problem(&e));
        }
        Err(e) => return Err(failed(st, &e.to_string())),
    };
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
    .unwrap_or(i64::MAX);
    let identity = match provider.verify_id_token(&id_token, &nonce, now).await {
        Ok(i) => i,
        Err(e @ OidcError::Unreachable(_)) => return Err(idp_problem(&e)),
        Err(e) => return Err(failed(st, &e.to_string())),
    };
    let mut tx = pool.begin().await?;
    let user_id: String = db::query_scalar(
        "INSERT INTO users (id, issuer, subject, email, name) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (issuer, subject) DO UPDATE SET email = EXCLUDED.email, name = EXCLUDED.name \
         RETURNING id",
    )
    .bind(ulid::Ulid::generate().to_string())
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .bind(&identity.email)
    .bind(&identity.name)
    .fetch_one(&mut *tx)
    .await?;
    // Always a fresh session id: nothing the browser held before survives.
    let session = auth::random_secret("")?;
    db::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at) \
         VALUES ($1, $2, now() + make_interval(secs => $3::bigint))",
    )
    .bind(auth::hash(&session))
    .bind(&user_id)
    .bind(auth::SESSION_SECS)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Entry {
            org_id: None,
            actor: &user_id,
            action: "auth.login",
            target: Some(&user_id),
            request_id: &auth::request_id(headers),
        },
        json!({ "issuer": identity.issuer }),
    )
    .await?;
    tx.commit().await?;
    st.metrics
        .inc("wyrd_hub_logins_total", &[("outcome", "success")]);
    tracing::info!(user_id, "login");
    Ok((user_id, session, return_to))
}

fn login_cookies(
    st: &AppState,
    session: &str,
) -> AppendHeaders<[(header::HeaderName, axum::http::HeaderValue); 2]> {
    AppendHeaders([
        (
            header::SET_COOKIE,
            auth::set_cookie(
                st,
                auth::session_cookie_name(st),
                session,
                auth::SESSION_SECS,
            ),
        ),
        (
            header::SET_COOKIE,
            auth::set_cookie(st, auth::login_cookie_name(st), "", 0),
        ),
    ])
}

/// The provider redirects the browser here.
async fn callback_get(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(p): Query<CallbackParams>,
) -> Result<Response, Problem> {
    let (_, session, return_to) = finish(&st, &headers, p).await?;
    Ok((
        StatusCode::SEE_OTHER,
        login_cookies(&st, &session),
        [(header::LOCATION, return_to)],
    )
        .into_response())
}

/// The same for a client that receives the redirect itself and posts
/// `{code, state}`.
async fn callback_post(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(p): Json<CallbackParams>,
) -> Result<Response, Problem> {
    let (user_id, session, return_to) = finish(&st, &headers, p).await?;
    Ok((
        login_cookies(&st, &session),
        Json(json!({ "user_id": user_id, "return_to": return_to })),
    )
        .into_response())
}

async fn logout(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    if caller.via == Via::Session
        && let Some(id) = auth::cookie(&headers, auth::session_cookie_name(&st))
    {
        let pool = st.db()?;
        let mut tx = pool.begin().await?;
        db::query("DELETE FROM sessions WHERE id_hash = $1")
            .bind(auth::hash(id))
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            Entry {
                org_id: None,
                actor: &caller.user_id,
                action: "auth.logout",
                target: Some(&caller.user_id),
                request_id: &caller.request_id,
            },
            json!({}),
        )
        .await?;
        tx.commit().await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            auth::set_cookie(&st, auth::session_cookie_name(&st), "", 0),
        )],
    )
        .into_response())
}

async fn me(
    State(st): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<serde_json::Value>, Problem> {
    let pool = st.db()?;
    let (email, name): (Option<String>, Option<String>) =
        db::query_as("SELECT email, name FROM users WHERE id = $1")
            .bind(&caller.user_id)
            .fetch_one(pool)
            .await?;
    let orgs: Vec<(String, String, String)> = db::query_as(
        "SELECT o.slug, o.name, m.role FROM memberships m JOIN orgs o ON o.id = m.org_id \
         WHERE m.user_id = $1 AND ($2::text IS NULL OR o.id = $2) ORDER BY o.slug",
    )
    .bind(&caller.user_id)
    .bind(&caller.token_org)
    .fetch_all(pool)
    .await?;
    Ok(Json(json!({
        "id": caller.user_id,
        "email": email,
        "name": name,
        "orgs": orgs.into_iter().map(|(slug, name, role)| json!({"slug": slug, "name": name, "role": role})).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_is_local_only() {
        assert_eq!(safe_return_to(Some("/orgs/acme")), "/orgs/acme");
        for bad in [
            "https://evil",
            "//evil",
            "/\\evil",
            "evil",
            "/a\r\nSet-Cookie: x",
        ] {
            assert_eq!(safe_return_to(Some(bad)), "/", "{bad:?}");
        }
        assert_eq!(safe_return_to(None), "/");
    }
}
