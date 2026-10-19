#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

//! The `OpenAPI` document against the real router: every route is
//! documented (and nothing else), and real responses match their schemas.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrdd::AppState;
use wyrdd::openapi::{check, document, routes};

/// (method, path) pairs registered in `routes/mod.rs`, read from source so
/// a new route cannot be added without documenting it.
fn registered() -> BTreeSet<(String, String)> {
    let src = include_str!("../src/routes/mod.rs");
    let mut out = BTreeSet::new();
    for chunk in src.split(".route(").skip(1) {
        let path = chunk.split('"').nth(1).unwrap().to_string();
        let call = chunk.split(".route(").next().unwrap();
        for m in ["get", "post", "patch", "delete", "put"] {
            if call.contains(&format!("{m}(")) {
                out.insert((m.to_string(), path.clone()));
            }
        }
    }
    out
}

#[test]
fn every_route_is_documented_and_nothing_else() {
    let documented: BTreeSet<_> = routes()
        .into_iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    assert_eq!(registered(), documented);
    let doc = document();
    for (m, p) in &documented {
        assert!(doc["paths"][p][m].is_object(), "{m} {p}");
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
}

fn env() -> Env {
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
            name: Some("contract".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    std::fs::write(s.dir().upper("a").join("a.txt"), "edited\n").unwrap();
    s.checkpoint(None).unwrap();
    let id = s.record().id.clone();
    let mut l = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    l.append(wyrd_correlator::synth::session(2).events).unwrap();
    Env {
        _tmp: tmp,
        data,
        id,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, String, Value) {
    let req = Request::builder().method(method).uri(path);
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let ct = res.headers()["content-type"].to_str().unwrap().to_string();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        ct,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn real_responses_match_their_schemas() {
    let e = env();
    let app = wyrdd::unix_app(Arc::new(AppState::new(
        e.data.clone(),
        "/nonexistent".into(),
    )));
    let doc = document();
    let sid = e.id.clone();
    let graph = call(&app, "GET", &format!("/v1/sessions/{sid}/graph"), None)
        .await
        .2;
    let tcid = graph["calls"][0]["call_id"].as_str().unwrap().to_string();
    let cases: Vec<(&str, String, &str, Option<Value>)> = vec![
        ("GET", "/v1/health".into(), "/v1/health", None),
        ("GET", "/v1/ready".into(), "/v1/ready", None),
        ("GET", "/v1/capabilities".into(), "/v1/capabilities", None),
        ("GET", "/v1/sessions".into(), "/v1/sessions", None),
        (
            "GET",
            format!("/v1/sessions/{sid}"),
            "/v1/sessions/{sid}",
            None,
        ),
        (
            "GET",
            format!("/v1/sessions/{sid}/events?limit=50"),
            "/v1/sessions/{sid}/events",
            None,
        ),
        (
            "GET",
            format!("/v1/sessions/{sid}/graph"),
            "/v1/sessions/{sid}/graph",
            None,
        ),
        (
            "GET",
            format!("/v1/sessions/{sid}/tool-calls/{tcid}"),
            "/v1/sessions/{sid}/tool-calls/{tcid}",
            None,
        ),
        (
            "GET",
            format!("/v1/sessions/{sid}/verify"),
            "/v1/sessions/{sid}/verify",
            None,
        ),
        (
            "POST",
            format!("/v1/sessions/{sid}/checkpoints"),
            "/v1/sessions/{sid}/checkpoints",
            None,
        ),
        (
            "GET",
            format!("/v1/sessions/{sid}/checkpoints"),
            "/v1/sessions/{sid}/checkpoints",
            None,
        ),
        (
            "GET",
            format!("/v1/sessions/{sid}/diff/c1/c2"),
            "/v1/sessions/{sid}/diff/{a}/{b}",
            None,
        ),
        (
            "POST",
            format!("/v1/sessions/{sid}/stop"),
            "/v1/sessions/{sid}/stop",
            None,
        ),
        (
            "POST",
            format!("/v1/sessions/{sid}/checkpoints/c1/apply"),
            "/v1/sessions/{sid}/checkpoints/{target}/apply",
            Some(json!({ "dry_run": true })),
        ),
        (
            "POST",
            format!("/v1/sessions/{sid}/checkpoints/c1/rewind"),
            "/v1/sessions/{sid}/checkpoints/{target}/rewind",
            None,
        ),
        (
            "POST",
            format!("/v1/sessions/{sid}/checkpoints/c1/fork"),
            "/v1/sessions/{sid}/checkpoints/{target}/fork",
            Some(json!({ "name": "contract" })),
        ),
        // Problems.
        (
            "GET",
            "/v1/sessions/zzzz".into(),
            "/v1/sessions/{sid}",
            None,
        ),
        (
            "POST",
            format!("/v1/sessions/{sid}/checkpoints/c1/fork"),
            "/v1/sessions/{sid}/checkpoints/{target}/fork",
            Some(json!({ "name": "contract" })),
        ),
    ];
    for (method, url, template, body) in cases {
        let (status, ct, value) = call(&app, method, &url, body).await;
        let op = &doc["paths"][template][method.to_ascii_lowercase()];
        let response = &op["responses"][status.to_string()];
        assert!(
            response.is_object(),
            "{method} {url}: status {status} is not documented: {value}"
        );
        let media = ct.split(';').next().unwrap();
        let schema = &response["content"][media]["schema"];
        assert!(
            schema.is_object(),
            "{method} {url}: {media} not documented for {status}"
        );
        check(&doc, schema, &value, "")
            .unwrap_or_else(|e| panic!("{method} {url} ({status}): {e}\n{value}"));
    }
}
