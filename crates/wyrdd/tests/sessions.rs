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
            policy: None,
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
    wyrdd::unix_app(Arc::new(AppState::new(
        e.data.clone(),
        "/nonexistent/wyrd".into(),
    )))
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

#[tokio::test]
async fn metrics_reflect_sessions_ledgers_and_open_streams() {
    let e = env();
    let id = recorded(&e);
    let app = app(&e);
    let metrics = |app: axum::Router| async move {
        let res = app
            .oneshot(Request::get("/v1/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    };
    let text = metrics(app.clone()).await;
    assert!(
        text.contains("wyrdd_sessions{status=\"preparing\"} 1"),
        "{text}"
    );
    let bytes: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix("wyrdd_ledger_bytes "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(bytes > 4096, "{bytes}");
    assert!(text.contains("wyrdd_sse_streams 0"));

    let stream = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(metrics(app.clone()).await.contains("wyrdd_sse_streams 1"));
    drop(stream);
    assert!(metrics(app).await.contains("wyrdd_sse_streams 0"));
}

/// One metric line's value (`name{labels} value`).
fn metric(text: &str, series: &str) -> f64 {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("{series} missing in\n{text}"))
        .parse()
        .unwrap()
}

#[tokio::test]
async fn metrics_count_ledger_numbers_and_follow_new_events() {
    let e = env();
    let id = recorded(&e);
    let app = app(&e);
    let scrape = |app: axum::Router| async move {
        let res = app
            .oneshot(Request::get("/v1/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    };
    let dir = e.data.session(&id);
    let head = || {
        let seq = wyrd_ledger::Ledger::open_read_only(dir.ledger())
            .unwrap()
            .head()
            .0;
        f64::from(u32::try_from(seq).unwrap())
    };

    let text = scrape(app.clone()).await;
    let events: f64 = wyrd_schema::Plane::ALL
        .iter()
        .map(|p| {
            metric(
                &text,
                &format!("wyrdd_ledger_events_total{{plane=\"{}\"}}", p.as_str()),
            )
        })
        .sum();
    assert_eq!(events, head(), "{text}");
    assert_eq!(metric(&text, "wyrdd_checkpoint_duration_ms_count"), 1.0);
    assert_eq!(
        metric(&text, "wyrdd_checkpoint_duration_ms_bucket{le=\"+Inf\"}"),
        1.0
    );
    assert_eq!(metric(&text, "wyrdd_metrics_unreadable_ledgers"), 0.0);
    assert_eq!(metric(&text, "wyrdd_judge_runs_total"), 0.0);
    assert!(!text.contains("wyrdd_denials_total{"), "{text}");

    // A denied connect and a judge run after the first scrape show up in
    // the next one.
    let mut ledger = wyrd_ledger::Ledger::open(dir.ledger()).unwrap();
    ledger
        .append(vec![wyrd_schema::Event {
            seq: 0,
            ts_mono_ns: 1,
            ts_wall_ms: 0,
            process: None,
            drops_since_last: 2,
            body: wyrd_schema::EventBody::NetConnect(wyrd_schema::NetConnect {
                family: 4,
                addr: "203.0.113.50".into(),
                port: 80,
                dns_name: None,
                verdict: wyrd_schema::NetVerdict::Denied,
            }),
        }])
        .unwrap();
    drop(ledger);
    std::fs::write(
        dir.root().join(wyrd_divergence::llm::JUDGE_RUNS),
        r#"{"model":"m","prompt_version":"v1","usage":{"input_tokens":100,"output_tokens":20},"est_cost_usd":0.25,"latency_ms":50,"attempts":1,"dropped":0,"error":null}"#,
    )
    .unwrap();
    let before = metric(&text, "wyrdd_ledger_dropped_events_total");
    let text = scrape(app.clone()).await;
    assert_eq!(
        metric(&text, "wyrdd_denials_total{kind=\"effect.net.connect\"}"),
        1.0,
        "{text}"
    );
    assert_eq!(
        metric(&text, "wyrdd_ledger_dropped_events_total"),
        before + 2.0
    );
    assert_eq!(
        metric(&text, "wyrdd_judge_tokens_total{type=\"input\"}"),
        100.0
    );
    assert_eq!(metric(&text, "wyrdd_judge_cost_usd_total"), 0.25);

    // A third scrape with nothing new reports the same totals.
    let events = |t: &str| {
        t.lines()
            .filter(|l| l.starts_with("wyrdd_ledger_events_total{"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(events(&scrape(app).await), events(&text));
}

#[tokio::test]
async fn metrics_skip_an_unreadable_ledger_and_say_so() {
    let e = env();
    let id = recorded(&e);
    let ledger = e.data.session(&id).ledger();
    for suffix in ["-wal", "-shm"] {
        let mut p = ledger.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    std::fs::write(&ledger, b"not a database").unwrap();
    let res = app(&e)
        .oneshot(Request::get("/v1/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(
        metric(&text, "wyrdd_metrics_unreadable_ledgers"),
        1.0,
        "{text}"
    );
    assert_eq!(metric(&text, "wyrdd_checkpoint_duration_ms_count"), 0.0);
}
