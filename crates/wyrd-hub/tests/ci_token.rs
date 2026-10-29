//! `POST /api/v1/ci/token` refusals need no database: the token is
//! verified against the issuer's keys first. Runs without Docker.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::actions::{self, Spoil};
use serde_json::json;
use wyrd_hub::config::{S3Config, Secret};
use wyrd_hub::oidc::Keys;
use wyrd_hub::{AppState, Storage, app};

/// A Hub whose database never connects and whose bucket is a closed port,
/// trusting `issuer` for CI tokens. The keys refresh without a rate limit.
fn offline(issuer: &str) -> Arc<AppState> {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let s3 = S3Config {
        endpoint: format!("http://127.0.0.1:{port}"),
        bucket: "wyrd-test".into(),
        region: "us-east-1".into(),
        access_key_id: "x".into(),
        secret_access_key: Secret::new(common::throwaway()),
    };
    let mut config = common::config("postgres://nowhere.invalid/hub", s3.clone());
    config.ci.issuer = issuer.to_string();
    let mut state = AppState::new(config, Storage::new(s3));
    state.ci = Keys::new(issuer.to_string()).with_refresh_interval(Duration::ZERO);
    Arc::new(state)
}

async fn exchange(app: &axum::Router, token: &str, org: &str) -> common::Reply {
    common::send(
        app,
        Request::post("/api/v1/ci/token")
            .header("content-type", "application/json")
            .body(Body::from(json!({"token": token, "org": org}).to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn spoiled_tokens_are_401_without_detail() {
    let gh = actions::start("http://hub.test").await;
    let state = offline(&gh.issuer);
    let app = app(state.clone());
    for spoil in [
        Spoil::Audience,
        Spoil::Issuer,
        Spoil::Expired,
        Spoil::NotYetValid,
        Spoil::Signature,
        Spoil::ForeignRepository,
    ] {
        let r = exchange(&app, &gh.token("acme/app", spoil), "acme").await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "{spoil:?}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "ci-token-rejected", "{spoil:?}");
        assert!(
            r.json().get("detail").is_none(),
            "{spoil:?}: no reason leaks"
        );
    }
    // Not a JWT at all; `alg: none`.
    for bad in [
        "nope",
        "a.b.c",
        &format!("{}.{}.", "eyJhbGciOiJub25lIn0", "e30"),
    ] {
        let r = exchange(&app, bad, "acme").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad}");
    }
    let m = common::get(&app, "/metrics").await.text();
    assert!(
        m.contains("wyrd_hub_ci_token_exchanges_total{outcome=\"failure\"} 9"),
        "{m}"
    );
}

#[tokio::test]
async fn bad_requests_are_400() {
    let gh = actions::start("http://hub.test").await;
    let app = app(offline(&gh.issuer));
    let r = exchange(&app, &gh.token("acme/app", Spoil::None), "Not A Slug").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = common::send(
        &app,
        Request::post("/api/v1/ci/token")
            .header("content-type", "application/json")
            .body(Body::from("{"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn issuer_unreachable_or_failing_is_503() {
    // Nothing listens at the configured issuer.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let issuer = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let gh = actions::start("http://hub.test").await;
    let down = app(offline(&issuer));
    let r = exchange(&down, &gh.token("acme/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["type"], "unavailable");
    // The issuer is up but its JWKS endpoint fails.
    gh.keys_down.store(true, Ordering::SeqCst);
    let failing = app(offline(&gh.issuer));
    let r = exchange(&failing, &gh.token("acme/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
}

#[tokio::test]
async fn a_valid_token_reaches_the_database_step_and_keys_rotate() {
    let gh = actions::start("http://hub.test").await;
    let app = app(offline(&gh.issuer));
    // Verified, then 503 because this Hub has no database: proves the
    // signature and claims passed.
    let r = exchange(&app, &gh.token("acme/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["detail"], "the database is not connected yet");
    // The issuer rotates its key: the Hub refreshes the JWKS on the
    // unknown kid and still verifies.
    gh.rotate();
    let r = exchange(&app, &gh.token("acme/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["detail"], "the database is not connected yet");
    // Keys are cached: with the JWKS endpoint down, known keys still work.
    gh.keys_down.store(true, Ordering::SeqCst);
    let r = exchange(&app, &gh.token("acme/app", Spoil::None), "acme").await;
    assert_eq!(r.json()["detail"], "the database is not connected yet");
}
