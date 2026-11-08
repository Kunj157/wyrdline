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
use wyrdd::AppState;

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
            policy: None,
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
    wyrdd::unix_app(Arc::new(AppState::new(
        e.data.clone(),
        "/nonexistent/wyrd".into(),
    )))
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
            Err(elapsed) => panic!("{elapsed}: got {} messages", parse(&buf).len()),
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

/// Hold the session lock as `Session::close` does, after saving the record as
/// closed: the record reads closed, and the closing change is still to come.
fn closing_window(e: &Env, s: &Session) -> std::fs::File {
    let state = s.dir().state();
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    record["status"] = "closed".into();
    std::fs::write(&state, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let _ = e;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(s.dir().lock())
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    lock
}

fn state_change(to: wyrd_schema::SessionState) -> wyrd_schema::Event {
    use wyrd_schema::{Event, EventBody, SessionState, SessionStateChanged};
    let from = if to == SessionState::Closed {
        SessionState::Closing
    } else {
        SessionState::Recording
    };
    Event {
        seq: 0,
        ts_mono_ns: 0,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body: EventBody::SessionStateChanged(SessionStateChanged {
            from,
            to,
            reason: None,
        }),
    }
}

#[tokio::test]
async fn the_stream_delivers_the_closing_change_after_the_record_reads_closed() {
    // `close` saves the record as Closed before it appends the change that
    // announces it. A stream that ends on the record alone sends `end` before
    // that change exists, so the final event is dropped. The window is built
    // here: the closing change is appended three seconds after the record reads
    // closed, while the session lock is held as `close` holds it.
    use wyrd_schema::SessionState;
    let e = env();
    let s = create(&e);
    let id = s.record().id.clone();
    let lock = closing_window(&e, &s);
    let app = app(&e);
    let path = format!("/v1/sessions/{id}/stream");

    let writer = {
        let data = e.data.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_secs(3));
            let s = Session::open(&data, &id).unwrap();
            let head = wyrd_ledger::Ledger::open(s.dir().ledger())
                .unwrap()
                .append(vec![state_change(SessionState::Closed)])
                .unwrap();
            drop(lock);
            head
        })
    };
    let msgs = read(&app, &path, None, |m| m.iter().any(|x| x.0 == "end")).await;
    let head = writer.await.unwrap();
    assert_eq!(
        ids(&msgs).last(),
        Some(&head),
        "the closing change is streamed before end"
    );
    assert_eq!(msgs.last().unwrap().0, "end");
}

#[tokio::test]
async fn a_close_after_an_earlier_failure_streams_its_closing_change() {
    // The session failed earlier, so its ledger already has a change to Failed.
    // Closing it later must still stream the change to Closed, not stop at the
    // old one.
    use wyrd_schema::SessionState;
    let e = env();
    let s = create(&e);
    let id = s.record().id.clone();
    wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(vec![state_change(SessionState::Failed)])
        .unwrap();
    let lock = closing_window(&e, &s);
    let app = app(&e);
    let path = format!("/v1/sessions/{id}/stream");
    let writer = {
        let data = e.data.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_secs(3));
            let s = Session::open(&data, &id).unwrap();
            let head = wyrd_ledger::Ledger::open(s.dir().ledger())
                .unwrap()
                .append(vec![state_change(SessionState::Closed)])
                .unwrap();
            drop(lock);
            head
        })
    };
    let msgs = read(&app, &path, None, |m| m.iter().any(|x| x.0 == "end")).await;
    let head = writer.await.unwrap();
    assert_eq!(ids(&msgs).last(), Some(&head));
}

#[tokio::test]
async fn a_closed_session_whose_closing_change_never_came_still_ends() {
    // A crash between saving the record as closed and appending the change
    // leaves no change and no lock. The stream must end, not hang.
    let e = env();
    let s = create(&e);
    let id = s.record().id.clone();
    let state = s.dir().state();
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    record["status"] = "closed".into();
    std::fs::write(&state, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let app = app(&e);
    let msgs = read(&app, &format!("/v1/sessions/{id}/stream"), None, |m| {
        m.iter().any(|x| x.0 == "end")
    })
    .await;
    assert_eq!(msgs.last().unwrap().0, "end");
}

/// Stress for the closing race on the stream: many sessions, each closed while
/// an SSE reader is attached, with the close varied against the poll. Counts
/// streams whose final event was not sent. Ignored: run with `--run-ignored all`;
/// `STREAM_STRESS_ROUNDS` sets the round count.
///
/// This does not reproduce the race on origin/dev: 0 of 1000 rounds missed the
/// final event, before and after the fix. The window between saving the record
/// and appending the closing change is too short for an in-process reader to
/// hit. The reproduction is the window tests above, which hold the session lock
/// while the record reads closed. Keep this as a check that the stream still
/// ends cleanly under churn, not as evidence of the fix.
#[tokio::test]
#[ignore = "stress: run with --run-ignored all"]
async fn stream_stress_closing_race() {
    let rounds: u64 = std::env::var("STREAM_STRESS_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let mut missed = 0;
    for i in 0..rounds {
        let e = env();
        let s = create(&e);
        let id = s.record().id.clone();
        let app = app(&e);
        let path = format!("/v1/sessions/{id}/stream");
        let closer = {
            let data = e.data.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis((i * 7919) % 250));
                let mut s = Session::open(&data, &id).unwrap();
                s.close("stress").unwrap();
            })
        };
        let msgs = read(&app, &path, None, |m| m.iter().any(|x| x.0 == "end")).await;
        closer.await.unwrap();
        let h = head(&Session::open(&e.data, &id).unwrap());
        if ids(&msgs).last() != Some(&h) {
            missed += 1;
            eprintln!(
                "round {i}: stream ended at {:?}, head {h}",
                ids(&msgs).last()
            );
        }
    }
    eprintln!("stream stress: {missed} of {rounds} rounds missed the final event");
    assert_eq!(missed, 0);
}
