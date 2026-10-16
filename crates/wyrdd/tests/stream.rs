#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The SSE stream against a real session ledger.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
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
    Env {
        data: DataDir::new(tmp.path().join("data")),
        ws,
        _tmp: tmp,
    }
}

fn create(e: &Env) -> Session {
    Session::create(
        &e.data,
        CreateOptions {
            workspace: e.ws.clone(),
            command: vec!["agent".into()],
            observe: false,
            name: None,
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap()
}

fn append(s: &Session, seed: u64) {
    let mut l = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    l.append(wyrd_correlator::synth::session(seed).events)
        .unwrap();
}

fn head(s: &Session) -> u64 {
    wyrd_ledger::Ledger::open_read_only(s.dir().ledger())
        .unwrap()
        .head()
        .0
}

fn app(e: &Env) -> axum::Router {
    wyrdd::unix_app(Arc::new(AppState {
        data: e.data.clone(),
        wyrd_bin: "/nonexistent/wyrd".into(),
        metrics: Metrics::default(),
    }))
}

/// Parsed SSE messages: (event name, id, data).
type Msg = (String, Option<u64>, String);

fn parse(buf: &str) -> Vec<Msg> {
    buf.split("\n\n")
        .filter(|m| !m.trim().is_empty() && !m.starts_with(':'))
        .map(|m| {
            let (mut ev, mut id, mut data) = (String::new(), None, String::new());
            for line in m.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    ev = v.into();
                } else if let Some(v) = line.strip_prefix("id: ") {
                    id = v.parse().ok();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data.push_str(v);
                }
            }
            (ev, id, data)
        })
        .collect()
}

/// Open the stream and read until `until` holds for the messages so far
/// (or the stream ends), with a deadline.
async fn read(
    app: &axum::Router,
    path: &str,
    last_event_id: Option<u64>,
    until: impl Fn(&[Msg]) -> bool,
) -> Vec<Msg> {
    let mut req = Request::get(path);
    if let Some(id) = last_event_id {
        req = req.header("last-event-id", id.to_string());
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let mut body = res.into_body();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let msgs = parse(&buf);
        if until(&msgs) {
            return msgs;
        }
        match tokio::time::timeout_at(deadline, body.frame()).await {
            Ok(Some(Ok(f))) => {
                if let Ok(d) = f.into_data() {
                    buf.push_str(&String::from_utf8_lossy(&d));
                }
            }
            Ok(_) => return parse(&buf),
            Err(_) => panic!("stream deadline; got {:?}", parse(&buf).len()),
        }
    }
}

fn ids(msgs: &[Msg]) -> Vec<u64> {
    msgs.iter()
        .filter(|m| m.0 == "ledger")
        .filter_map(|m| m.1)
        .collect()
}

#[tokio::test]
async fn resumes_after_last_event_id_exactly_once() {
    let e = env();
    let s = create(&e);
    append(&s, 3);
    let id = s.record().id.clone();
    let n = head(&s);
    let app = app(&e);
    let path = format!("/v1/sessions/{id}/stream");

    // First connection: read part of the history, then "disconnect".
    let first = read(&app, &path, None, |m| ids(m).len() >= 5).await;
    let got = ids(&first);
    assert_eq!(got[..5], [1, 2, 3, 4, 5]);
    let last = got[4];
    let data: serde_json::Value = serde_json::from_str(&first[0].2).unwrap();
    assert_eq!(data["seq"], 1);

    // Reconnect with Last-Event-ID: every later event once, in order.
    let rest = read(&app, &path, Some(last), |m| ids(m).last() == Some(&n)).await;
    assert_eq!(ids(&rest), ((last + 1)..=n).collect::<Vec<_>>());
}

#[tokio::test]
async fn live_events_arrive_and_a_closed_session_ends_the_stream() {
    let e = env();
    let s = create(&e);
    let id = s.record().id.clone();
    let before = head(&s);
    let app = app(&e);
    let path = format!("/v1/sessions/{id}/stream?after_seq={before}");

    let writer = {
        let data = e.data.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(400));
            let mut s = Session::open(&data, &id).unwrap();
            append(&s, 4);
            std::thread::sleep(Duration::from_millis(400));
            s.close("test").unwrap();
        })
    };
    let msgs = read(&app, &path, None, |m| m.iter().any(|x| x.0 == "end")).await;
    writer.await.unwrap();
    let got = ids(&msgs);
    assert!(got.len() > 5, "live events streamed: {}", got.len());
    assert_eq!(got.first(), Some(&(before + 1)));
    assert!(
        got.windows(2).all(|w| w[1] == w[0] + 1),
        "dense and ordered"
    );
    let end = msgs.last().unwrap();
    assert_eq!(end.0, "end");
    let s = Session::open(&e.data, &id).unwrap();
    assert_eq!(end.2, head(&s).to_string());
}

#[tokio::test]
async fn unknown_sessions_and_bad_resume_points_are_rejected() {
    let e = env();
    let s = create(&e);
    let app = app(&e);
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/sessions/zzzz/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app
        .oneshot(
            Request::get(format!("/v1/sessions/{}/stream", s.record().id))
                .header("last-event-id", "nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
