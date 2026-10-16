#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The daemon API driven through the real router (TCP flavour, in memory)
//! and over a real unix socket.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;
use wyrd_cell::DataDir;
use wyrdd::{AppState, Metrics};

const TOKEN: &str = "0123456789abcdef";

fn state(dir: &std::path::Path) -> Arc<AppState> {
    Arc::new(AppState {
        data: DataDir::new(dir.join("data")),
        wyrd_bin: "/nonexistent/wyrd".into(),
        metrics: Metrics::default(),
    })
}

fn tcp(dir: &std::path::Path) -> axum::Router {
    wyrdd::tcp_app(state(dir), TOKEN, wyrdd::cors(wyrdd::DEFAULT_LISTEN, None))
}

async fn send(
    app: axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn tcp_requires_the_bearer_token() {
    let dir = tempfile::tempdir().unwrap();
    for token in [None, Some("wrong"), Some("0123456789abcdeF")] {
        let (status, headers, body) = send(tcp(dir.path()), get("/v1/health", token)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
        assert_eq!(headers[header::CONTENT_TYPE], "application/problem+json");
        let p: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(p["type"], "unauthorized");
        assert_eq!(p["status"], 401);
        assert!(headers.contains_key("x-request-id"));
    }
    let (status, _, body) = send(tcp(dir.path()), get("/v1/health", Some(TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"ok\""));
}

#[tokio::test]
async fn request_ids_are_echoed_or_generated() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = get("/v1/health", Some(TOKEN));
    req.headers_mut()
        .insert("x-request-id", "client-42".parse().unwrap());
    let (_, headers, _) = send(tcp(dir.path()), req).await;
    assert_eq!(headers["x-request-id"], "client-42");
    let (_, headers, _) = send(tcp(dir.path()), get("/v1/health", Some(TOKEN))).await;
    assert_eq!(headers["x-request-id"].len(), 26, "a ULID");
}

#[tokio::test]
async fn unknown_routes_are_problems() {
    let dir = tempfile::tempdir().unwrap();
    let (status, headers, _) = send(tcp(dir.path()), get("/v1/nope", Some(TOKEN))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[header::CONTENT_TYPE], "application/problem+json");
}

#[tokio::test]
async fn cors_admits_only_the_studio_origin() {
    let dir = tempfile::tempdir().unwrap();
    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/v1/health")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .body(Body::empty())
            .unwrap()
    };
    let (_, headers, _) = send(tcp(dir.path()), preflight("http://127.0.0.1:7717")).await;
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://127.0.0.1:7717"
    );
    let (_, headers, _) = send(tcp(dir.path()), preflight("https://evil.example")).await;
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
}

#[tokio::test]
async fn ready_and_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let app = tcp(dir.path());
    let (status, _, _) = send(app.clone(), get("/v1/ready", Some(TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, _) = send(app.clone(), get("/v1/ready", None)).await;
    let (status, headers, body) = send(app, get("/v1/metrics", Some(TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    assert!(
        body.contains("wyrdd_http_requests_total{class=\"2xx\"} 1"),
        "{body}"
    );
    assert!(
        body.contains("wyrdd_http_requests_total{class=\"4xx\"} 1"),
        "{body}"
    );
}

#[tokio::test]
async fn ready_fails_when_the_data_dir_is_unusable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data"), b"a file, not a directory").unwrap();
    let (status, _, body) = send(tcp(dir.path()), get("/v1/ready", Some(TOKEN))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("not-ready"));
}

#[tokio::test]
async fn unix_socket_serves_the_same_user_without_a_token() {
    use std::os::unix::fs::PermissionsExt as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("run/wyrdd.sock");
    let listener = wyrdd::listen::SameUidListener::bind(&sock).unwrap();
    assert_eq!(
        std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // A second daemon on the same live socket is refused.
    assert!(wyrdd::listen::SameUidListener::bind(&sock).is_err());
    let app = wyrdd::unix_app(state(dir.path()));
    tokio::spawn(axum::serve(listener, app).into_future());
    let mut s = tokio::net::UnixStream::connect(&sock).await.unwrap();
    s.write_all(b"GET /v1/health HTTP/1.1\r\nhost: wyrdd\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.to_ascii_lowercase().contains("x-request-id:"));
}
