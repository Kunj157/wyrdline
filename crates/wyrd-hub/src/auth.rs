//! Who is calling: browser sessions (cookie) and API tokens (bearer),
//! roles, and the checks that guard cookie-authenticated mutations.

use std::sync::Arc;

use axum::extract::{FromRequestParts, MatchedPath};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};

use crate::db::{self, PgPool};
use crate::{AppState, Problem, REQUEST_ID};

/// Lifetime of a browser session.
pub const SESSION_SECS: i64 = 7 * 24 * 3600;
/// Prefix of API tokens, so leaked ones are recognisable.
pub const TOKEN_PREFIX: &str = "wyrdhub_";
/// Prefix of upload-only tokens minted for CI (`POST /api/v1/ci/token`).
pub const CI_TOKEN_PREFIX: &str = "wyrdci_";

/// Organisation roles, ordered by power.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Member,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "member" => Some(Role::Member),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// 32 random bytes, base64url (43 characters), behind `prefix`.
pub fn random_secret(prefix: &str) -> Result<String, Problem> {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Problem::internal(&"system random generator failed"))?;
    Ok(format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// Hex SHA-256: how session ids, tokens, and login state are stored.
pub fn hash(secret: &str) -> String {
    hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        secret.as_bytes(),
    ))
}

/// `scheme://host[:port]` of a URL.
pub fn origin_of(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |i| i + 3);
    url[after_scheme..]
        .find('/')
        .map_or(url, |i| &url[..after_scheme + i])
}

/// Cookies are `Secure` (and use the `__Host-` prefix) when the Hub is
/// served over HTTPS.
fn secure(state: &AppState) -> bool {
    state.config.public_url.starts_with("https://")
}

pub fn session_cookie_name(state: &AppState) -> &'static str {
    if secure(state) {
        "__Host-wyrd_session"
    } else {
        "wyrd_session"
    }
}

pub fn login_cookie_name(state: &AppState) -> &'static str {
    if secure(state) {
        "__Host-wyrd_login"
    } else {
        "wyrd_login"
    }
}

/// A `Set-Cookie` value: `HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` on HTTPS;
/// `max_age` 0 deletes the cookie.
pub fn set_cookie(state: &AppState, name: &str, value: &str, max_age: i64) -> HeaderValue {
    let secure = if secure(state) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .unwrap_or(HeaderValue::from_static(""))
}

/// The value of cookie `name`.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// The request id (set on every request by the observe middleware).
pub fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(&REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// How the caller authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Session,
    Token,
}

/// What a credential may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// Everything the user's roles allow.
    #[default]
    Full,
    /// Only the bundle upload of the token's organisation (CI tokens).
    Upload,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "full" => Some(Scope::Full),
            "upload" => Some(Scope::Upload),
            _ => None,
        }
    }
}

/// An authenticated caller.
#[derive(Debug, Clone)]
pub struct Caller {
    pub user_id: String,
    /// A token restricted to one organisation (its id).
    pub token_org: Option<String>,
    pub via: Via,
    pub scope: Scope,
    pub request_id: String,
}

/// Whether an upload-only token may call `method` on the matched route
/// template `route` at `path`: the bundle upload of its own organisation
/// (`org_slug`), and who-am-i.
pub fn upload_scope_allows(method: &Method, route: &str, path: &str, org_slug: &str) -> bool {
    if org_slug.is_empty() {
        return false;
    }
    let segments = |s: &str| s.split('/').map(str::to_string).collect::<Vec<_>>();
    match (method, segments(route).as_slice()) {
        (&Method::GET, [_, api, v1, me]) => api == "api" && v1 == "v1" && me == "me",
        (&Method::POST, [_, api, v1, orgs, org, bundles]) => {
            api == "api"
                && v1 == "v1"
                && orgs == "orgs"
                && org.starts_with('{')
                && bundles == "bundles"
                && segments(path).get(4).is_some_and(|s| s == org_slug)
        }
        _ => false,
    }
}

fn unauthorized() -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Sign in or send a valid API token",
    )
}

/// Browsers send `Origin` on cross-site and same-site mutations; a
/// cookie-authenticated mutation must come from the Hub's own origin.
pub(crate) fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) => origin == origin_of(&state.config.public_url),
        None => headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s == "same-origin"),
    }
}

/// A live token: user, organisation (id and slug), and scope.
type TokenRow = (String, Option<String>, Option<String>, String);

async fn by_token(pool: &PgPool, token: &str) -> Result<Option<TokenRow>, Problem> {
    let row: Option<TokenRow> = db::query_as(
        "UPDATE api_tokens t SET last_used_at = now() \
         WHERE token_hash = $1 AND (expires_at IS NULL OR expires_at > now()) \
         RETURNING user_id, org_id, (SELECT slug FROM orgs WHERE id = t.org_id), scope",
    )
    .bind(hash(token))
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

async fn by_session(pool: &PgPool, id: &str) -> Result<Option<String>, Problem> {
    Ok(
        db::query_scalar("SELECT user_id FROM sessions WHERE id_hash = $1 AND expires_at > now()")
            .bind(hash(id))
            .fetch_optional(pool)
            .await?,
    )
}

impl FromRequestParts<Arc<AppState>> for Caller {
    type Rejection = Problem;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let pool = state.db()?;
        let request_id = request_id(&parts.headers);
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if let Some(token) = bearer {
            let (user_id, token_org, org_slug, scope) = by_token(pool, token.trim())
                .await?
                .ok_or_else(unauthorized)?;
            let scope = Scope::parse(&scope).ok_or_else(unauthorized)?;
            if scope == Scope::Upload {
                let route = parts
                    .extensions
                    .get::<MatchedPath>()
                    .map_or("", MatchedPath::as_str);
                let slug = org_slug.as_deref().unwrap_or_default();
                if !upload_scope_allows(&parts.method, route, parts.uri.path(), slug) {
                    return Err(Problem::new(
                        StatusCode::FORBIDDEN,
                        "insufficient-scope",
                        "This token may only upload bundles to its organisation",
                    ));
                }
            }
            return Ok(Caller {
                user_id,
                token_org,
                via: Via::Token,
                scope,
                request_id,
            });
        }
        let id = cookie(&parts.headers, session_cookie_name(state)).ok_or_else(unauthorized)?;
        let user_id = by_session(pool, id).await?.ok_or_else(unauthorized)?;
        let safe = matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
        if !safe && !same_origin(state, &parts.headers) {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                "cross-site",
                "Cross-site request refused",
            ));
        }
        Ok(Caller {
            user_id,
            token_org: None,
            via: Via::Session,
            scope: Scope::Full,
            request_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_order_and_parse() {
        assert!(Role::Viewer < Role::Member && Role::Member < Role::Admin);
        for r in [Role::Viewer, Role::Member, Role::Admin] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("owner"), None);
    }

    #[test]
    fn secrets_are_random_and_hashed() {
        let a = random_secret(TOKEN_PREFIX).unwrap();
        let b = random_secret(TOKEN_PREFIX).unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(TOKEN_PREFIX));
        assert_eq!(a.len(), TOKEN_PREFIX.len() + 43);
        assert_eq!(hash(&a).len(), 64);
        assert_ne!(hash(&a), hash(&b));
    }

    #[test]
    fn upload_scope_is_one_route_of_one_org() {
        let upload = "/api/v1/orgs/{org}/bundles";
        assert!(upload_scope_allows(
            &Method::POST,
            upload,
            "/api/v1/orgs/acme/bundles",
            "acme"
        ));
        // Another organisation, another route, another method.
        assert!(!upload_scope_allows(
            &Method::POST,
            upload,
            "/api/v1/orgs/other/bundles",
            "acme"
        ));
        assert!(!upload_scope_allows(
            &Method::GET,
            upload,
            "/api/v1/orgs/acme/bundles",
            "acme"
        ));
        assert!(!upload_scope_allows(
            &Method::DELETE,
            "/api/v1/bundles/{bid}",
            "/api/v1/bundles/b1",
            "acme"
        ));
        assert!(!upload_scope_allows(
            &Method::GET,
            "/api/v1/orgs/{org}/ci-trust",
            "/api/v1/orgs/acme/ci-trust",
            "acme"
        ));
        assert!(upload_scope_allows(
            &Method::GET,
            "/api/v1/me",
            "/api/v1/me",
            "acme"
        ));
        // No route template (unmatched) or no organisation: nothing.
        assert!(!upload_scope_allows(
            &Method::POST,
            "",
            "/api/v1/orgs/acme/bundles",
            "acme"
        ));
        assert!(!upload_scope_allows(
            &Method::POST,
            upload,
            "/api/v1/orgs//bundles",
            ""
        ));
        // Read endpoints, the analytics ones included, are never open to an
        // upload-only token: only the upload and who-am-i pass the allow-list.
        for (method, path) in [
            (Method::GET, "/api/v1/orgs/acme/stats"),
            (Method::GET, "/api/v1/orgs/acme/search"),
            (Method::GET, "/api/v1/orgs/acme/bundles"),
        ] {
            let route = path.replace("acme", "{org}");
            assert!(
                !upload_scope_allows(&method, &route, path, "acme"),
                "{method} {path}"
            );
        }
        assert_eq!(Scope::parse("upload"), Some(Scope::Upload));
        assert_eq!(Scope::parse("admin"), None);
    }

    #[test]
    fn origins_and_cookies() {
        assert_eq!(
            origin_of("https://hub.example.com/x/y"),
            "https://hub.example.com"
        );
        assert_eq!(origin_of("http://127.0.0.1:7730"), "http://127.0.0.1:7730");
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; wyrd_session=abc"),
        );
        h.append(header::COOKIE, HeaderValue::from_static("b=2"));
        assert_eq!(cookie(&h, "wyrd_session"), Some("abc"));
        assert_eq!(cookie(&h, "b"), Some("2"));
        assert_eq!(cookie(&h, "wyrd"), None);
    }
}
