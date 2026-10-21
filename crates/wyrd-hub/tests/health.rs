//! Liveness, readiness, request ids, and metrics without any backing
//! service: the Hub must answer and say what is missing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use wyrd_hub::config::{S3Config, Secret};
use wyrd_hub::{AppState, Storage, app};

/// A Hub whose database never connects and whose bucket is a closed port.
fn offline() -> axum::Router {
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
    let config = common::config("postgres://nowhere.invalid/hub", s3.clone());
    app(Arc::new(AppState::new(config, Storage::new(s3))))
}

#[tokio::test]
async fn healthz_answers_without_dependencies() {
    let r = common::get(&offline(), "/healthz").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["status"], "ok");
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    assert_eq!(r.headers["cache-control"], "no-store");
}

#[tokio::test]
async fn readyz_is_503_and_names_what_is_missing() {
    let r = common::get(&offline(), "/readyz").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = r.json();
    assert_eq!(body["ready"], false);
    assert_eq!(body["checks"][0]["name"], "database");
    assert_eq!(body["checks"][0]["error"], "not connected yet");
    assert_eq!(body["checks"][1]["name"], "storage");
    assert_eq!(body["checks"][1]["ok"], false);
}

#[tokio::test]
async fn request_ids_are_kept_or_generated() {
    let app = offline();
    let r = common::send(
        &app,
        Request::get("/healthz")
            .header("x-request-id", "abc-123")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.headers["x-request-id"], "abc-123");
    let r = common::send(
        &app,
        Request::get("/healthz")
            .header("x-request-id", "bad id with spaces")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let id = r.headers["x-request-id"].to_str().unwrap();
    assert_eq!(id.len(), 26, "a ULID replaces an invalid id: {id}");
}

#[tokio::test]
async fn metrics_count_requests_by_route_template() {
    let app = offline();
    common::get(&app, "/healthz").await;
    common::get(&app, "/no/such/path").await;
    let r = common::get(&app, "/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    let text = r.text();
    assert!(
        text.contains(
            "wyrd_hub_http_requests_total{method=\"GET\",route=\"/healthz\",class=\"2xx\"} 1"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "wyrd_hub_http_requests_total{method=\"GET\",route=\"unmatched\",class=\"4xx\"} 1"
        ),
        "{text}"
    );
    assert!(text.contains("wyrd_hub_ready 0"), "{text}");
    assert!(
        !text.contains("/no/such/path"),
        "raw paths never become labels"
    );
}

#[tokio::test]
async fn retrying_connect_keeps_the_hub_up() {
    // A database that refuses connections: the retry loop keeps going
    // (and logs), it never panics or exits.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let url = format!("postgres://hub:x@127.0.0.1:{port}/hub");
    let task = tokio::spawn(async move { wyrd_hub::db::connect_with_retry(&url, 1).await });
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(!task.is_finished(), "still retrying");
    task.abort();
}

#[tokio::test]
async fn sync_needs_the_database_and_says_so() {
    // Authentication needs the database: an upgrade attempt before it is
    // connected is 503, never an open relay.
    let r = common::send(
        &offline(),
        Request::get("/api/v1/sync?bundle=b1")
            .header("authorization", "Bearer wyrdhub_nope")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    let text = common::get(&offline(), "/metrics").await.text();
    assert!(text.contains("wyrd_hub_sync_clients 0"), "{text}");
}
