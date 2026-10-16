#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Mutating endpoints against real sessions (unprivileged: the upper layer
//! is written directly, as overlayfs would leave it).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrdd::AppState;

struct Env {
    tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
}

/// A session with checkpoint c1 (a.txt edited, b.txt added).
fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws.clone(),
            command: vec!["agent".into()],
            observe: false,
            name: None,
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    let up = s.dir().upper("a");
    std::fs::write(up.join("a.txt"), "edited\n").unwrap();
    std::fs::write(up.join("b.txt"), "new\n").unwrap();
    s.checkpoint(None).unwrap();
    let id = s.record().id.clone();
    Env { tmp, data, id }
}

fn app(e: &Env, wyrd_bin: &Path) -> axum::Router {
    wyrdd::unix_app(Arc::new(AppState::new(e.data.clone(), wyrd_bin.into())))
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, v)
}

fn kinds(e: &Env) -> Vec<String> {
    let s = Session::open(&e.data, &e.id).unwrap();
    let l = wyrd_ledger::Ledger::open_read_only(s.dir().ledger()).unwrap();
    l.events_after(0, 10_000)
        .unwrap()
        .iter()
        .map(|ev| ev.kind().to_string())
        .collect()
}

#[tokio::test]
async fn checkpoint_is_idempotent_per_key() {
    let e = env();
    let app = app(&e, Path::new("/nonexistent"));
    let path = format!("/v1/sessions/{}/checkpoints", e.id);
    let (s1, h1, v1) = call(&app, "POST", &path, None, Some("k-1")).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(v1["id"], "c2");
    assert!(!h1.contains_key("idempotent-replayed"));
    let (s2, h2, v2) = call(&app, "POST", &path, None, Some("k-1")).await;
    assert_eq!((s2, v2.clone()), (StatusCode::CREATED, v1));
    assert_eq!(h2["idempotent-replayed"], "true");
    let (_, _, list) = call(&app, "GET", &path, None, None).await;
    assert_eq!(
        list["checkpoints"].as_array().unwrap().len(),
        2,
        "one effect"
    );
    // Same key, different request.
    let (s3, _, v3) = call(
        &app,
        "POST",
        &format!("/v1/sessions/{}/stop", e.id),
        None,
        Some("k-1"),
    )
    .await;
    assert_eq!(s3, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v3["type"], "idempotency-key-reused");
    // Without a key every request acts.
    call(&app, "POST", &path, None, None).await;
    let (_, _, v) = call(&app, "POST", &path, None, None).await;
    assert_eq!(v["id"], "c4");
}

#[tokio::test]
async fn rewind_fork_diff_and_stop() {
    let e = env();
    let app = app(&e, Path::new("/nonexistent"));
    let base = format!("/v1/sessions/{}", e.id);
    let (st, _, _) = call(&app, "POST", &format!("{base}/checkpoints"), None, None).await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, _, v) = call(
        &app,
        "POST",
        &format!("{base}/checkpoints/c1/rewind"),
        None,
        Some("r"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["branch"]["id"], "b");
    // Retried rewind does not create branch c.
    let (_, _, again) = call(
        &app,
        "POST",
        &format!("{base}/checkpoints/c1/rewind"),
        None,
        Some("r"),
    )
    .await;
    assert_eq!(again, v);

    let fork = format!("{base}/checkpoints/c1/fork");
    let (st, _, v) = call(&app, "POST", &fork, Some(json!({ "name": "exp" })), None).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(v["branch"]["name"], "exp");
    let (st, _, v) = call(&app, "POST", &fork, Some(json!({ "name": "exp" })), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, h, v) = call(&app, "POST", &fork, None, None).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "name required");
    assert_eq!(h["content-type"], "application/problem+json");
    assert_eq!(v["type"], "invalid-body");
    let (st, _, _) = call(
        &app,
        "POST",
        &fork,
        Some(json!({ "name": "x", "bogus": 1 })),
        None,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown fields rejected"
    );
    let (_, _, cps) = call(&app, "GET", &format!("{base}/checkpoints"), None, None).await;
    assert_eq!(cps["current_branch"], "c");
    assert_eq!(cps["branches"].as_array().unwrap().len(), 3);

    let (st, _, v) = call(&app, "GET", &format!("{base}/diff/c1/c2"), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["changes"].is_array());
    let (st, _, v) = call(&app, "GET", &format!("{base}/diff/c1/..%2Fx"), None, None).await;
    assert_eq!(
        (st, v["type"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid-target"))
    );
    let (st, _, v) = call(
        &app,
        "POST",
        &format!("{base}/checkpoints/c99/rewind"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    let (_, _, v) = call(&app, "POST", &format!("{base}/stop"), None, None).await;
    assert_eq!(v["stopped"], false);

    let k = kinds(&e);
    assert_eq!(
        k.iter()
            .filter(|k| *k == "state.checkpoint.rewound")
            .count(),
        1
    );
    assert_eq!(
        k.iter().filter(|k| *k == "state.checkpoint.forked").count(),
        1
    );
}

#[tokio::test]
async fn relaunch_is_handed_to_a_detached_wyrd() {
    use std::os::unix::fs::PermissionsExt as _;
    let e = env();
    let fake = e.tmp.path().join("fake-wyrd");
    let out = e.tmp.path().join("args.txt");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$@\" > {}.tmp\nmv {0}.tmp {0}\n",
            out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let app = app(&e, &fake);
    let (st, _, v) = call(
        &app,
        "POST",
        &format!("/v1/sessions/{}/checkpoints/c1/rewind", e.id),
        Some(json!({ "relaunch": true })),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert!(v["pid"].as_u64().is_some());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !out.exists() {
        assert!(Instant::now() < deadline, "fake wyrd never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        format!("rewind {} c1 --relaunch", e.id)
    );
    // Nothing was rewound by the daemon itself.
    let (_, _, cps) = call(
        &app,
        "GET",
        &format!("/v1/sessions/{}/checkpoints", e.id),
        None,
        None,
    )
    .await;
    assert_eq!(cps["current_branch"], "a");
}
