//! OIDC login against a test provider, sessions, and logout (Postgres
//! and MinIO in containers).

#![cfg(feature = "container-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::idp::{self, Spoil};
use wyrd_hub::db;

#[tokio::test]
async fn login_creates_user_session_and_audit_row() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let s = common::start_login(&app, "/orgs/acme").await;
    assert!(s.authorize.starts_with(&format!("{}/auth?", idp.issuer)));
    assert!(s.authorize.contains("code_challenge_method=S256"));
    let login_set = common::get(&app, "/api/v1/auth/oidc/login").await;
    let raw = login_set.headers["set-cookie"].to_str().unwrap();
    assert!(
        raw.contains("HttpOnly") && raw.contains("SameSite=Lax"),
        "{raw}"
    );

    let cb = common::approve(&idp, &s.authorize).await;
    let r = common::callback(&app, &cb, &s.login_cookie).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(common::location(&r), "/orgs/acme");
    let session = common::set_cookie(&r.headers, "wyrd_session").unwrap();
    assert_eq!(
        common::set_cookie(&r.headers, "wyrd_login").as_deref(),
        Some("wyrd_login="),
        "the binding cookie is cleared"
    );

    let me = common::send(&app, common::as_user("GET", "/api/v1/me", &session, None)).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text());
    assert_eq!(me.json()["email"], "alice@example.test");
    let user_id = me.json()["id"].as_str().unwrap().to_string();

    let actions: Vec<(String, Option<String>)> =
        db::query_as("SELECT action, request_id FROM audit_logs WHERE actor = $1")
            .bind(&user_id)
            .fetch_all(&hub.pool)
            .await
            .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].0, "auth.login");
    assert!(actions[0].1.is_some(), "audit rows carry the request id");

    // Logging in again: same user, a new session.
    let again = common::login(&app, &idp, "alice").await;
    assert_ne!(again, session);
    let me2 = common::send(&app, common::as_user("GET", "/api/v1/me", &again, None)).await;
    assert_eq!(me2.json()["id"], user_id.as_str());
    let m = common::get(&app, "/metrics").await.text();
    assert!(
        m.contains("wyrd_hub_logins_total{outcome=\"success\"} 2"),
        "{m}"
    );
    // Session ids are stored hashed.
    let raw = session.strip_prefix("wyrd_session=").unwrap();
    let stored: i64 = db::query_scalar("SELECT count(*) FROM sessions WHERE id_hash = $1")
        .bind(raw)
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn bad_state_browser_or_replay_is_401_without_detail() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let s = common::start_login(&app, "/").await;
    let cb = common::approve(&idp, &s.authorize).await;
    // Another browser (no or a different binding cookie).
    for cookie in ["", "wyrd_login=someone-else"] {
        let r = common::callback(&app, &cb, cookie).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.json()["type"], "login-failed");
        assert!(r.json().get("detail").is_none());
    }
    // The state was consumed by the failed attempt: replay fails too.
    let r = common::callback(&app, &cb, &s.login_cookie).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Unknown state.
    let s = common::start_login(&app, "/").await;
    let cb = common::approve(&idp, &s.authorize).await;
    let forged = cb.replace("state=", "state=x");
    let r = common::callback(&app, &forged, &s.login_cookie).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Provider error.
    let r = common::callback(
        &app,
        "/api/v1/auth/oidc/callback?error=access_denied",
        &s.login_cookie,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let m = common::get(&app, "/metrics").await.text();
    assert!(
        m.contains("wyrd_hub_logins_total{outcome=\"failure\"} 5"),
        "{m}"
    );
}

#[tokio::test]
async fn spoiled_id_tokens_are_refused() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    for spoil in [
        Spoil::Nonce,
        Spoil::Audience,
        Spoil::Issuer,
        Spoil::Expired,
        Spoil::Signature,
    ] {
        *idp.spoil.lock().unwrap() = spoil;
        let s = common::start_login(&app, "/").await;
        let cb = common::approve(&idp, &s.authorize).await;
        let r = common::callback(&app, &cb, &s.login_cookie).await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "{spoil:?}: {}",
            r.text()
        );
    }
    let users: i64 = db::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[tokio::test]
async fn post_callback_and_open_redirects() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let s = common::start_login(&app, "https://evil.example/").await;
    let cb = common::approve(&idp, &s.authorize).await;
    let q: std::collections::HashMap<String, String> =
        serde_urlencoded_pairs(cb.split_once('?').unwrap().1);
    let r = common::send(
        &app,
        Request::post("/api/v1/auth/oidc/callback")
            .header("cookie", &s.login_cookie)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"code": q["code"], "state": q["state"]}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["return_to"], "/", "absolute URLs never come back");
    assert!(common::set_cookie(&r.headers, "wyrd_session").is_some());
}

fn serde_urlencoded_pairs(q: &str) -> std::collections::HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn logout_needs_same_origin_and_ends_the_session() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let session = common::login(&app, &idp, "bob").await;
    let cross = common::send(
        &app,
        Request::post("/api/v1/auth/logout")
            .header("cookie", &session)
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    let no_origin = common::send(
        &app,
        Request::post("/api/v1/auth/logout")
            .header("cookie", &session)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(no_origin.status, StatusCode::FORBIDDEN);
    let out = common::send(
        &app,
        common::as_user("POST", "/api/v1/auth/logout", &session, None),
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let me = common::send(&app, common::as_user("GET", "/api/v1/me", &session, None)).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unauthenticated_and_provider_down() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    assert_eq!(
        common::get(&app, "/api/v1/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    let bogus = common::send(
        &app,
        Request::get("/api/v1/me")
            .header("authorization", "Bearer wyrdhub_nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(bogus.status, StatusCode::UNAUTHORIZED);
    // A Hub whose provider is not listening: login is 503, not a crash.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let issuer = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let mut cfg = hub.state.config.clone();
    cfg.oidc = Some(wyrd_hub::config::OidcConfig {
        issuer,
        client_id: "x".into(),
        client_secret: wyrd_hub::config::Secret::new(common::throwaway()),
    });
    let down = std::sync::Arc::new(wyrd_hub::AppState::new(
        cfg,
        wyrd_hub::Storage::new(hub.minio.s3.clone()),
    ));
    down.set_db(hub.pool.clone());
    let r = common::get(&wyrd_hub::app(down), "/api/v1/auth/oidc/login").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn login_disabled_without_a_provider() {
    let hub = common::hub().await;
    let r = common::get(&hub.app(), "/api/v1/auth/oidc/login").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
