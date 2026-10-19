#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Studio answers a live session's asks through the daemon, which uses the
//! session's control token; the intent socket is served here by
//! `wyrd-adapters`, as `wyrd run` does.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_adapters::asks::{Asks, CONTROL_TOKEN_FILE};
use wyrd_adapters::{Ingest, Sink, StaticPolicy, ToolPolicy};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{CaptureMode, Decision, EventBody};
use wyrdd::AppState;

const TOKEN: &str = "daemon-token-for-tests";
const CONTROL: &str = "c0ffee";

#[derive(Default)]
struct Mem(Mutex<Vec<EventBody>>);

impl Sink for Mem {
    fn append(&self, bodies: Vec<EventBody>) -> Result<(), String> {
        self.0.lock().unwrap().extend(bodies);
        Ok(())
    }
}

#[derive(Debug)]
struct AlwaysAsk;

impl ToolPolicy for AlwaysAsk {
    fn id(&self) -> &'static str {
        "always-ask"
    }
    fn version(&self) -> &'static str {
        "b3:00"
    }
    fn on_tool_call(&self, _: &str, _: &Value, _: &str, _: u32) -> (Decision, String) {
        (Decision::Ask, "please confirm".into())
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
            policy: None,
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

/// Serve the intent socket with an ask registry, like `wyrd run`.
fn live(e: &Env) -> Arc<Ingest<Mem>> {
    let s = Session::open(&e.data, &e.id).unwrap();
    std::fs::write(s.dir().root().join(CONTROL_TOKEN_FILE), CONTROL).unwrap();
    let path = wyrd_adapters::server::socket_path(s.dir().root(), &e.id);
    let listener = wyrd_adapters::server::bind(&path).unwrap();
    let ingest = Arc::new(
        Ingest::new(
            Mem::default(),
            CaptureMode::Cell,
            StaticPolicy::default(),
            0,
        )
        .with_policy(Arc::new(AlwaysAsk))
        .with_asks(Arc::new(Asks::new(CONTROL.into(), Duration::from_secs(10)))),
    );
    let served = ingest.clone();
    std::thread::spawn(move || wyrd_adapters::server::serve(&listener, &served));
    ingest
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
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
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
async fn a_session_that_is_not_running_has_no_asks() {
    let e = env();
    let app = app(&e);
    let (s, v) = send(&app, "GET", &format!("/v1/sessions/{}/asks", e.id), None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"asks": []})));
    let (s, v) = send(
        &app,
        "POST",
        &format!("/v1/sessions/{}/asks/1", e.id),
        Some(json!({"allow": true})),
    )
    .await;
    assert_eq!(
        (s, v["type"].as_str()),
        (StatusCode::NOT_FOUND, Some("not-found"))
    );
    let (s, _) = send(&app, "GET", "/v1/sessions/nope/asks", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn studio_lists_and_answers_asks() {
    let e = env();
    let ingest = live(&e);
    let app = app(&e);
    for allow in [true, false] {
        let worker = {
            let i = ingest.clone();
            std::thread::spawn(move || {
                i.handle(wyrd_adapters::Request::Hook {
                    adapter: "claude-code".into(),
                    payload: json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": "t1", "tool_input": {"command": "curl x"}}),
                })
            })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let id = loop {
            let (s, v) = send(&app, "GET", &format!("/v1/sessions/{}/asks", e.id), None).await;
            assert_eq!(s, StatusCode::OK);
            if let Some(a) = v["asks"].as_array().and_then(|a| a.first()) {
                assert_eq!(a["reason"], "please confirm");
                assert_eq!(a["policy_id"], "always-ask");
                break a["id"].as_u64().unwrap();
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        // A body with unknown fields is refused.
        let (s, _) = send(
            &app,
            "POST",
            &format!("/v1/sessions/{}/asks/{id}", e.id),
            Some(json!({"allow": true, "by": "agent"})),
        )
        .await;
        assert!(s.is_client_error());
        let (s, v) = send(
            &app,
            "POST",
            &format!("/v1/sessions/{}/asks/{id}", e.id),
            Some(json!({"allow": allow})),
        )
        .await;
        assert_eq!((s, v), (StatusCode::OK, json!({"id": id, "allow": allow})));
        let wyrd_adapters::Response::Decision { decision, reason } = worker.join().unwrap() else {
            panic!()
        };
        assert_eq!(
            decision,
            if allow {
                Decision::Allow
            } else {
                Decision::Deny
            }
        );
        if !allow {
            assert!(reason.unwrap().contains("(studio,"));
        }
        // Answering twice is a 404.
        let (s, _) = send(
            &app,
            "POST",
            &format!("/v1/sessions/{}/asks/{id}", e.id),
            Some(json!({"allow": true})),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn asks_need_the_daemon_token() {
    let e = env();
    let app = app(&e);
    let req = Request::get(format!("/v1/sessions/{}/asks", e.id))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
