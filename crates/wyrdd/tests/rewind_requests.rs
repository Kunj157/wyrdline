#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Studio decides the agent's rewind requests through the daemon, with
//! the user's daemon token; the session's control token (the only secret
//! a running session hands out) does not open these routes.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrdd::AppState;

const TOKEN: &str = "daemon-token-for-tests";
const CONTROL: &str = "c0ffee";

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
}

/// A Cell-mode session with checkpoints c1, c2 and two pending requests
/// to c1 (made as `wyrd mcp` does).
fn env() -> (Env, u64, u64) {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into()],
            observe: false,
            name: None,
            agent_kind: "claude-code".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    s.checkpoint(None).unwrap();
    std::fs::write(s.dir().upper("a").join("b.txt"), "new\n").unwrap();
    s.checkpoint(None).unwrap();
    std::fs::write(
        s.dir().root().join(wyrd_adapters::asks::CONTROL_TOKEN_FILE),
        CONTROL,
    )
    .unwrap();
    let r1 = s.request_rewind("c1", "broke the tests", "mcp").unwrap().id;
    let r2 = s.request_rewind("c1", "please go back", "mcp").unwrap().id;
    let id = s.record().id.clone();
    (
        Env {
            _tmp: tmp,
            data,
            id,
        },
        r1,
        r2,
    )
}

fn app(e: &Env) -> axum::Router {
    let st = Arc::new(AppState::new(e.data.clone(), "/nonexistent".into()));
    wyrdd::tcp_app(
        st,
        Arc::new(wyrdd::auth::TokenSource::fixed(TOKEN)),
        wyrdd::cors(wyrdd::DEFAULT_LISTEN, None),
    )
}

async fn send_as(
    app: &axum::Router,
    bearer: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(b) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send_as(app, Some(TOKEN), method, path, body).await
}

fn branches(e: &Env) -> usize {
    Session::open(&e.data, &e.id)
        .unwrap()
        .record()
        .branches
        .len()
}

#[tokio::test]
async fn deciding_needs_the_daemon_token_not_the_control_token() {
    let (e, r1, _) = env();
    let app = app(&e);
    let path = format!("/v1/sessions/{}/rewind-requests/{r1}", e.id);
    for bearer in [None, Some(CONTROL), Some("wrong")] {
        let (s, _) = send_as(&app, bearer, "POST", &path, Some(json!({"confirm": true}))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{bearer:?}");
        let (s, _) = send_as(
            &app,
            bearer,
            "GET",
            &format!("/v1/sessions/{}/rewind-requests", e.id),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{bearer:?}");
    }
    assert_eq!(branches(&e), 1, "nothing was rewound");
}

#[tokio::test]
async fn studio_lists_rejects_and_confirms_requests() {
    let (e, r1, r2) = env();
    let app = app(&e);
    let list = format!("/v1/sessions/{}/rewind-requests", e.id);
    let (s, v) = send(&app, "GET", &list, None).await;
    assert_eq!(s, StatusCode::OK);
    let reqs = v["requests"].as_array().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0]["id"], r1);
    assert_eq!(reqs[0]["status"], "pending");
    assert_eq!(reqs[0]["reason"], "broke the tests");
    assert_eq!(reqs[0]["source"], "mcp");

    // Unknown fields and unknown requests are refused.
    let one = |id: u64| format!("/v1/sessions/{}/rewind-requests/{id}", e.id);
    let (s, _) = send(
        &app,
        "POST",
        &one(r1),
        Some(json!({"confirm": true, "by": "agent"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, v) = send(&app, "POST", &one(999_999), Some(json!({"confirm": true}))).await;
    assert_eq!(
        (s, v["type"].as_str()),
        (StatusCode::NOT_FOUND, Some("not-found"))
    );

    let (s, v) = send(
        &app,
        "POST",
        &one(r1),
        Some(json!({"confirm": false, "note": "keep going"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["request"]["status"], "rejected");
    assert_eq!(v["request"]["decided_by"], "studio");
    assert_eq!(v["request"]["note"], "keep going");
    assert_eq!(v["branch"], Value::Null);
    assert_eq!(branches(&e), 1);

    // Decided once: a second decision conflicts.
    let (s, _) = send(&app, "POST", &one(r1), Some(json!({"confirm": true}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(branches(&e), 1);

    let (s, v) = send(&app, "POST", &one(r2), Some(json!({"confirm": true}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["request"]["status"], "confirmed");
    assert_eq!(v["branch"]["forked_from"], "c1");
    let rec = Session::open(&e.data, &e.id).unwrap();
    assert_eq!(
        rec.record().current_branch,
        v["branch"]["id"].as_str().unwrap()
    );
}

#[tokio::test]
async fn an_unknown_session_is_not_found() {
    let (e, _, _) = env();
    let app = app(&e);
    let (s, _) = send(&app, "GET", "/v1/sessions/nope/rewind-requests", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
