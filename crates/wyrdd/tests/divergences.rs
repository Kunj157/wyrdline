#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Divergences over the API: listed with their triage state, triaged into
//! the ledger, refused once the ledger is sealed.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{DivergenceRaised, Event, EventBody, Severity};
use wyrdd::AppState;

const TOKEN: &str = "daemon-token-for-tests";

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    sid: String,
}

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
            body: EventBody::DivergenceRaised(DivergenceRaised {
                divergence_id: "dv-0123456789ab".into(),
                rule_id: "D003".into(),
                severity: Severity::Critical,
                claim_id: None,
                evidence_seqs: vec![1],
                confidence_bp: 9500,
                summary: "read .env, then connected to diag.example.net within 10 s".into(),
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
    body: Option<Value>,
    auth: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if auth {
        req = req.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
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

#[tokio::test]
async fn list_and_triage() {
    let e = env();
    let app = app(&e);
    let list = format!("/v1/sessions/{}/divergences", e.sid);
    let (s, v) = send(&app, "GET", &list, None, true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["divergences"][0]["divergence_id"], "dv-0123456789ab");
    assert_eq!(v["divergences"][0]["rule_name"], "secret-read-then-egress");
    assert_eq!(v["divergences"][0]["status"], "open");

    let one = format!("{list}/dv-0123456789ab");
    let (s, v) = send(
        &app,
        "PATCH",
        &one,
        Some(json!({"status": "acknowledged", "note": "real"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "acknowledged");
    assert_eq!(v["note"], "real");
    let (_, v) = send(&app, "GET", &list, None, true).await;
    assert_eq!(v["divergences"][0]["status"], "acknowledged");

    // The update is a ledger event.
    let s = Session::open(&e.data, &e.sid).unwrap();
    let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger()).unwrap();
    let newest = ledger.events_after(0, 100).unwrap().pop().unwrap();
    assert_eq!(newest.kind(), "verdict.divergence.updated");
}

#[tokio::test]
async fn triage_errors() {
    let e = env();
    let app = app(&e);
    let base = format!("/v1/sessions/{}/divergences", e.sid);
    let (s, _) = send(
        &app,
        "PATCH",
        &format!("{base}/dv-ffffffffffff"),
        Some(json!({"status": "acknowledged"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let one = format!("{base}/dv-0123456789ab");
    let (s, _) = send(&app, "PATCH", &one, Some(json!({"status": "maybe"})), true).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = send(
        &app,
        "PATCH",
        &one,
        Some(json!({"status": "open", "note": "x".repeat(2_001)})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = send(
        &app,
        "PATCH",
        &one,
        Some(json!({"status": "acknowledged"})),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = send(&app, "GET", "/v1/sessions/NOPE/divergences", None, true).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // A sealed ledger takes no more events.
    Session::open(&e.data, &e.sid)
        .unwrap()
        .close("test")
        .unwrap();
    let (s, v) = send(
        &app,
        "PATCH",
        &one,
        Some(json!({"status": "acknowledged"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
}
