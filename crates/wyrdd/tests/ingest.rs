#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Hook and OTLP ingest: per-session tokens on TCP, forwarding to a live
//! session's intent socket (served here by `wyrd-adapters`, as `wyrd run`
//! does).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_adapters::{Ingest, Sink, StaticPolicy};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{CaptureMode, EventBody};
use wyrdd::AppState;

const TOKEN: &str = "daemon-token-for-tests";

#[derive(Default)]
struct Mem(Mutex<Vec<EventBody>>);

impl Sink for Mem {
    fn append(&self, bodies: Vec<EventBody>) -> Result<(), String> {
        self.0.lock().unwrap().extend(bodies);
        Ok(())
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
    let data = DataDir::new(tmp.path().join("data"));
    let s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into()],
            observe: false,
            name: None,
            agent_kind: "claude-code".into(),
            deny_tools: Vec::new(),
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    let id = s.record().id.clone();
    Env {
        _tmp: tmp,
        data,
        id,
    }
}

/// Serve the session's intent socket like `wyrd run` does.
fn live(e: &Env, deny: &[&str]) -> Arc<Ingest<Mem>> {
    let s = Session::open(&e.data, &e.id).unwrap();
    let path = wyrd_adapters::server::socket_path(s.dir().root(), &e.id);
    let listener = wyrd_adapters::server::bind(&path).unwrap();
    let ingest = Arc::new(Ingest::new(
        Mem::default(),
        CaptureMode::Cell,
        StaticPolicy {
            deny_tools: deny.iter().map(|d| (*d).to_string()).collect(),
        },
        0,
    ));
    let served = ingest.clone();
    std::thread::spawn(move || wyrd_adapters::server::serve(&listener, &served));
    ingest
}

fn tcp(e: &Env) -> axum::Router {
    let st = Arc::new(AppState::new(e.data.clone(), "/nonexistent".into()));
    wyrdd::tcp_app(st, TOKEN, wyrdd::cors(wyrdd::DEFAULT_LISTEN, None))
}

async fn post(
    app: &axum::Router,
    path: &str,
    token: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let req = Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, content_type)
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

fn tool_span() -> Value {
    let attr = |k: &str, v: &str| json!({"key": k, "value": {"stringValue": v}});
    json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": "5b8efff798038103d269b633813fc60c", "spanId": "a1", "name": "tool",
        "startTimeUnixNano": "1000", "endTimeUnixNano": "2000",
        "attributes": [
            attr("gen_ai.operation.name", "execute_tool"),
            attr("gen_ai.tool.name", "shell"),
            attr("gen_ai.tool.call.id", "call_1"),
        ],
        "status": {"code": 1}
    }]}]}]})
}

#[tokio::test]
async fn ingest_tokens_are_scoped_to_one_session_and_to_ingest() {
    let e = env();
    let app = tcp(&e);
    let mine = wyrdd::auth::ingest_token(TOKEN, &e.id);
    let path = format!("/v1/sessions/{}/otlp/v1/traces", e.id);
    let body = tool_span().to_string().into_bytes();
    // Valid token, but nothing is recording: authorised, then 409.
    let (s, v) = post(&app, &path, &mine, "application/json", body.clone()).await;
    assert_eq!(
        (s, v["type"].as_str()),
        (StatusCode::CONFLICT, Some("session-not-live"))
    );
    for bad in [
        "nope".to_string(),
        TOKEN.to_string(),
        wyrdd::auth::ingest_token(TOKEN, "01other"),
        wyrdd::auth::ingest_token("another-daemon", &e.id),
    ] {
        let (s, _) = post(&app, &path, &bad, "application/json", body.clone()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{bad}");
    }
    // The ingest token opens nothing else.
    let (s, _) = post(
        &app,
        &format!("/v1/sessions/{}/checkpoints", e.id),
        &mine,
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn otlp_and_hooks_reach_the_live_session() {
    let e = env();
    let ingest = live(&e, &["Bash"]);
    let app = tcp(&e);
    let token = wyrdd::auth::ingest_token(TOKEN, &e.id);

    let (s, v) = post(
        &app,
        &format!("/v1/sessions/{}/otlp/v1/traces", e.id),
        &token,
        "application/json",
        tool_span().to_string().into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({ "partialSuccess": {} }));
    let kinds: Vec<_> = ingest
        .sink()
        .0
        .lock()
        .unwrap()
        .iter()
        .map(EventBody::kind)
        .collect();
    assert_eq!(
        kinds,
        ["intent.tool_call.started", "intent.tool_call.finished"]
    );

    let hook = json!({
        "hook_event_name": "PreToolUse", "session_id": "x", "tool_use_id": "t1",
        "tool_name": "Bash", "tool_input": {"command": "ls"}
    });
    let (s, v) = post(
        &app,
        &format!("/v1/sessions/{}/ingest/hooks/claude-code", e.id),
        &token,
        "application/json",
        hook.to_string().into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["type"], "decision");
    assert_eq!(v["decision"], "deny");
}

#[tokio::test]
async fn protobuf_unknown_adapters_and_huge_bodies_are_refused() {
    let e = env();
    let app = tcp(&e);
    let token = wyrdd::auth::ingest_token(TOKEN, &e.id);
    let traces = format!("/v1/sessions/{}/otlp/v1/traces", e.id);
    let (s, v) = post(&app, &traces, &token, "application/x-protobuf", vec![0x0a]).await;
    assert_eq!(
        (s, v["type"].as_str()),
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Some("protobuf-unsupported")
        )
    );
    let (s, _) = post(
        &app,
        &format!("/v1/sessions/{}/ingest/hooks/nope", e.id),
        &token,
        "application/json",
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = post(
        &app,
        &traces,
        &token,
        "application/json",
        vec![b' '; 9 * 1024 * 1024],
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}
