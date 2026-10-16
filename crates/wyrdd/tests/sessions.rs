#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Session read endpoints against real sessions created with `wyrd-cell`
//! (no privileges: the upper layer is written directly) and a ledger
//! filled with a synthetic agent run.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use serde_json::Value;
use tower::ServiceExt as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrdd::{AppState, Metrics};

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    ws: std::path::PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    Env {
        data: DataDir::new(tmp.path().join("data")),
        ws,
        _tmp: tmp,
    }
}

/// A session whose ledger holds a synthetic run, plus one checkpoint.
fn recorded(e: &Env) -> String {
    let mut s = Session::create(
        &e.data,
        CreateOptions {
            workspace: e.ws.clone(),
            command: vec!["agent".into()],
            observe: false,
            name: Some("demo".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    let up = s.dir().upper(&s.record().current_branch);
    std::fs::write(up.join("a.txt"), "edited\n").unwrap();
    s.checkpoint(Some("s1c0".into())).unwrap();
    let id = s.record().id.clone();
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    ledger
        .append(wyrd_correlator::synth::session(1).events)
        .unwrap();
    id
}

fn app(e: &Env) -> axum::Router {
    wyrdd::unix_app(Arc::new(AppState {
        data: e.data.clone(),
        wyrd_bin: "/nonexistent/wyrd".into(),
        metrics: Metrics::default(),
    }))
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn list_and_filter_sessions() {
    let e = env();
    let id = recorded(&e);
    let app = app(&e);
    let (s, v) = get(&app, "/v1/sessions").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["sessions"][0]["id"], id.as_str());
    let ws = e.ws.canonicalize().unwrap();
    for (q, n) in [
        ("agent=fakeagent", 1),
        ("agent=codex", 0),
        (&format!("repo={}", ws.display()) as &str, 1),
        ("repo=/elsewhere", 0),
        ("since=0", 1),
        ("since=99999999999999", 0),
        ("status=closed", 0),
    ] {
        let (_, v) = get(&app, &format!("/v1/sessions?{q}")).await;
        assert_eq!(v["sessions"].as_array().unwrap().len(), n, "{q}");
    }
}

#[tokio::test]
async fn session_detail_has_the_ledger_head() {
    let e = env();
    let id = recorded(&e);
    let (s, v) = get(&app(&e), &format!("/v1/sessions/{id}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["name"], "demo");
    assert!(v["head_seq"].as_u64().unwrap() > 10);
    assert!(v["head_hash"].as_str().unwrap().starts_with("b3:"));
    assert_eq!(v["sealed"], false);
}

#[tokio::test]
async fn event_pages_cover_every_event_exactly_once() {
    let e = env();
    let id = recorded(&e);
    let app = app(&e);
    let (_, head) = get(&app, &format!("/v1/sessions/{id}")).await;
    let head = head["head_seq"].as_u64().unwrap();
    let mut seen = Vec::new();
    let mut after = 0;
    loop {
        let (s, v) = get(
            &app,
            &format!("/v1/sessions/{id}/events?after_seq={after}&limit=7"),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let page = v["events"].as_array().unwrap();
        assert!(page.len() <= 7);
        seen.extend(page.iter().map(|e| e["seq"].as_u64().unwrap()));
        match v["next_after_seq"].as_u64() {
            Some(n) => after = n,
            None => break,
        }
    }
    assert_eq!(seen, (1..=head).collect::<Vec<_>>());

    let (_, v) = get(
        &app,
        &format!("/v1/sessions/{id}/events?plane=effect&limit=5000"),
    )
    .await;
    let effects = v["events"].as_array().unwrap();
    assert_ne!(effects.len(), 0);
    assert!(
        effects
            .iter()
            .all(|e| e["body"]["kind"].as_str().unwrap().starts_with("effect."))
    );
    let (_, v) = get(
        &app,
        &format!("/v1/sessions/{id}/events?kind=state.checkpoint"),
    )
    .await;
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn verify_and_checkpoints() {
    let e = env();
    let id = recorded(&e);
    let app = app(&e);
    let (s, v) = get(&app, &format!("/v1/sessions/{id}/verify")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], true);
    assert_eq!(v["sealed"], false);

    let (_, v) = get(&app, &format!("/v1/sessions/{id}/checkpoints")).await;
    assert_eq!(v["current_branch"], "a");
    assert_eq!(v["checkpoints"][0]["id"], "c1");
}

#[tokio::test]
async fn graph_and_tool_calls() {
    let e = env();
    let id = recorded(&e);
    let app = app(&e);
    let (s, g) = get(&app, &format!("/v1/sessions/{id}/graph")).await;
    assert_eq!(s, StatusCode::OK);
    let all_effects = g["effects"].as_array().unwrap().len();
    assert!(all_effects > 0 && !g["calls"].as_array().unwrap().is_empty());
    let first = g["effects"][0]["seq"].as_u64().unwrap();
    let (_, part) = get(
        &app,
        &format!("/v1/sessions/{id}/graph?from_seq={first}&to_seq={first}"),
    )
    .await;
    assert_eq!(part["effects"].as_array().unwrap().len(), 1);

    let call = g["calls"][0]["call_id"].as_str().unwrap().to_string();
    let (s, v) = get(&app, &format!("/v1/sessions/{id}/tool-calls/{call}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["call"]["call_id"], call.as_str());
    let (s, v) = get(&app, &format!("/v1/sessions/{id}/tool-calls/nope")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["type"], "not-found");
}

#[tokio::test]
async fn bad_unknown_and_ambiguous_ids_are_problems() {
    let e = env();
    recorded(&e);
    recorded(&e);
    let app = app(&e);
    for (path, status, kind) in [
        (
            "/v1/sessions/..%2Fetc",
            StatusCode::BAD_REQUEST,
            "invalid-id",
        ),
        ("/v1/sessions/ZZZ", StatusCode::BAD_REQUEST, "invalid-id"),
        ("/v1/sessions/zzzz", StatusCode::NOT_FOUND, "not-found"),
        ("/v1/sessions/0", StatusCode::CONFLICT, "ambiguous"),
        (
            "/v1/sessions/zzzz/events",
            StatusCode::NOT_FOUND,
            "not-found",
        ),
    ] {
        let (s, v) = get(&app, path).await;
        assert_eq!(s, status, "{path}");
        assert_eq!(v["type"], kind, "{path}");
    }
}
