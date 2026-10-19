#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Policy upload, listing, and backtests over the API.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{Event, EventBody, IntentSource, ToolCallStarted};
use wyrdd::AppState;

const TOKEN: &str = "daemon-token-for-tests";

fn hostile() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../policies/fixtures/hostile.wasm"
    ))
    .unwrap()
}

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    sid: String,
}

/// A session whose ledger holds one call the hostile policy denies.
fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into()],
            observe: true,
            name: None,
            agent_kind: "claude-code".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    ledger
        .append(vec![Event {
            seq: 0,
            ts_mono_ns: 1,
            ts_wall_ms: 0,
            process: None,
            drops_since_last: 0,
            body: EventBody::ToolCallStarted(ToolCallStarted {
                call_id: "c1".into(),
                source: IntentSource::Hook,
                tool: "deny".into(),
                args: json!({}),
                turn: Some(1),
            }),
        }])
        .unwrap();
    Env {
        _tmp: tmp,
        sid: s.record().id.clone(),
        data,
    }
}

fn app(e: &Env) -> axum::Router {
    let st = Arc::new(AppState::new(e.data.clone(), "/nonexistent".into()));
    wyrdd::tcp_app(
        st,
        Arc::new(wyrdd::auth::TokenSource::fixed(TOKEN)),
        wyrdd::cors(wyrdd::DEFAULT_LISTEN, None),
    )
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    ct: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn upload_list_and_backtest() {
    let e = env();
    let app = app(&e);
    let (s, v) = send(&app, "GET", "/v1/policies", "application/json", Vec::new()).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"policies": []})));

    let (s, v) = send(
        &app,
        "POST",
        "/v1/policies?name=hostile",
        "application/wasm",
        hostile(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["id"], "hostile");
    let version = v["version"].as_str().unwrap().to_string();
    assert!(version.starts_with("b3:"));

    let (_, v) = send(&app, "GET", "/v1/policies", "application/json", Vec::new()).await;
    assert_eq!(v["policies"][0]["version"], version.as_str());

    let body = json!({"session_id": e.sid}).to_string().into_bytes();
    let (s, v) = send(
        &app,
        "POST",
        "/v1/policies/hostile/backtest",
        "application/json",
        body.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["policy_version"], version.as_str());
    assert_eq!(v["findings"].as_array().unwrap().len(), 1);
    assert_eq!(v["findings"][0]["stage"], "hook");
    assert_eq!(v["findings"][0]["decision"], "deny");
    // Deterministic.
    let (_, again) = send(
        &app,
        "POST",
        "/v1/policies/hostile/backtest",
        "application/json",
        body,
    )
    .await;
    assert_eq!(again, v);

    let req = Request::get("/v1/metrics")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let m =
        String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(m.contains("wyrdd_policy_backtests_total 2"), "{m}");
}

#[tokio::test]
async fn bad_uploads_and_unknowns_are_problems() {
    let e = env();
    let app = app(&e);
    let (s, v) = send(
        &app,
        "POST",
        "/v1/policies?name=bad",
        "application/wasm",
        b"not wasm".to_vec(),
    )
    .await;
    assert_eq!(
        (s, v["type"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid-policy"))
    );
    for name in ["..%2Fx", "", "a%20b", &"x".repeat(65)] {
        let (s, _) = send(
            &app,
            "POST",
            &format!("/v1/policies?name={name}"),
            "application/wasm",
            hostile(),
        )
        .await;
        assert!(s.is_client_error(), "{name}: {s}");
    }
    assert!(!e.data.policies().join("bad.wasm").exists());
    let body = json!({"session_id": e.sid}).to_string().into_bytes();
    let (s, _) = send(
        &app,
        "POST",
        "/v1/policies/nope/backtest",
        "application/json",
        body,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    send(
        &app,
        "POST",
        "/v1/policies?name=hostile",
        "application/wasm",
        hostile(),
    )
    .await;
    let body = json!({"session_id": "nope"}).to_string().into_bytes();
    let (s, _) = send(
        &app,
        "POST",
        "/v1/policies/hostile/backtest",
        "application/json",
        body,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let too_big = vec![0u8; wyrd_policy::MAX_COMPONENT_BYTES + 1];
    let (s, _) = send(
        &app,
        "POST",
        "/v1/policies?name=big",
        "application/wasm",
        too_big,
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}
