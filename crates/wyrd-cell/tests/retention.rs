#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd gc` retention (#318): opt-in expiry of closed sessions by count and
//! age. Running, busy, imported and open sessions are never expired, a dry run
//! moves nothing, and an unreadable record stops the collection before any
//! session is moved.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use wyrd_cell::gc::{self, Retention};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session, now_ms, state};

const DAY_MS: i64 = 86_400_000;

fn session(data: &DataDir, ws: &Path) -> Session {
    std::fs::create_dir_all(ws).unwrap();
    Session::create(
        data,
        CreateOptions {
            workspace: ws.to_path_buf(),
            command: vec!["true".into()],
            observe: false,
            name: None,
            agent_kind: "test".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap()
}

/// A closed session with one checkpoint of distinct content and the given age.
fn closed(data: &DataDir, root: &Path, name: &str, created_ms: i64) -> String {
    let mut s = session(data, &root.join(format!("ws-{name}")));
    std::fs::write(s.dir().upper("a").join(format!("{name}.txt")), name).unwrap();
    s.checkpoint(None).unwrap();
    s.close("done").unwrap();
    let mut rec = state::load(s.dir()).unwrap();
    rec.created_ms = created_ms;
    state::save(s.dir(), &rec).unwrap();
    s.record().id.clone()
}

/// An open session (never closed) with the given age.
fn open(data: &DataDir, root: &Path, name: &str, created_ms: i64) -> String {
    let s = session(data, &root.join(format!("ws-{name}")));
    let mut rec = state::load(s.dir()).unwrap();
    rec.created_ms = created_ms;
    state::save(s.dir(), &rec).unwrap();
    s.record().id.clone()
}

fn exists(data: &DataDir, id: &str) -> bool {
    data.session(id).root().exists()
}

fn keep(days: Option<u32>, sessions: Option<usize>) -> Retention {
    Retention {
        keep_days: days,
        keep_sessions: sessions,
    }
}

#[test]
fn retention_is_opt_in_and_with_no_limit_nothing_expires() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let a = closed(&data, tmp.path(), "a", 1_000);
    let c = gc::collect_retained(&data, false, Duration::ZERO, &Retention::default()).unwrap();
    assert!(c.expired.is_empty(), "{c:?}");
    assert!(exists(&data, &a));
}

#[test]
fn keep_sessions_expires_the_older_closed_session_and_its_chunks_go() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let a = closed(&data, tmp.path(), "a", now_ms() - 9 * DAY_MS);
    let b = closed(&data, tmp.path(), "b", now_ms() - DAY_MS);
    let o = open(&data, tmp.path(), "o", now_ms() - 20 * DAY_MS);
    let r = keep(None, Some(1));

    let dry = gc::collect_retained(&data, true, Duration::ZERO, &r).unwrap();
    assert_eq!(dry.expired, vec![a.clone()]);
    assert!(exists(&data, &a), "a dry run moves nothing");
    assert!(
        dry.report.deleted > 0,
        "a's chunks are collectable: {dry:?}"
    );

    let real = gc::collect_retained(&data, false, Duration::ZERO, &r).unwrap();
    assert_eq!(real.expired, vec![a.clone()]);
    assert!(!exists(&data, &a), "a is removed from sessions/");
    assert!(exists(&data, &b), "the newest closed session stays");
    assert!(exists(&data, &o), "an open session never expires");
    assert!(real.report.deleted > 0);
    assert!(
        gc::roots(&data).is_ok(),
        "every remaining session still reads"
    );
    assert_eq!(
        gc::dry_run(&data).unwrap().deleted,
        0,
        "nothing left to free"
    );
}

#[test]
fn keep_days_expires_only_closed_sessions_older_than_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let old = closed(&data, tmp.path(), "old", now_ms() - 10 * DAY_MS);
    let young = closed(&data, tmp.path(), "young", now_ms() - DAY_MS);
    let c = gc::collect_retained(&data, false, Duration::ZERO, &keep(Some(3), None)).unwrap();
    assert_eq!(c.expired, vec![old.clone()]);
    assert!(!exists(&data, &old));
    assert!(exists(&data, &young));
}

#[test]
fn with_both_limits_a_session_is_kept_if_either_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    // Beyond the count, but within the days: kept.
    let within_days = closed(&data, tmp.path(), "wd", now_ms() - 10 * DAY_MS);
    // Beyond both: expires.
    let beyond = closed(&data, tmp.path(), "bb", now_ms() - 40 * DAY_MS);
    let r = keep(Some(30), Some(0));
    let c = gc::collect_retained(&data, false, Duration::ZERO, &r).unwrap();
    assert_eq!(c.expired, vec![beyond]);
    assert!(exists(&data, &within_days));
}

#[test]
fn a_running_or_imported_session_is_never_expired() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let running = closed(&data, tmp.path(), "run", now_ms() - 5 * DAY_MS);
    let mut rec = state::load(&data.session(&running)).unwrap();
    rec.pid = Some(std::process::id());
    state::save(&data.session(&running), &rec).unwrap();
    let imported = closed(&data, tmp.path(), "imp", now_ms() - 5 * DAY_MS);
    std::fs::write(data.session(&imported).import(), b"{}").unwrap();
    let r = keep(None, Some(0));
    let c = gc::collect_retained(&data, false, Duration::ZERO, &r).unwrap();
    assert!(c.expired.is_empty(), "{c:?}");
    assert!(exists(&data, &running));
    assert!(exists(&data, &imported));
}

#[test]
fn a_session_whose_lock_is_held_is_kept_and_reported_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let busy = closed(&data, tmp.path(), "busy", now_ms() - 5 * DAY_MS);
    let _held = state::lock(&data.session(&busy).lock()).unwrap();
    let r = keep(None, Some(0));
    let dry = gc::collect_retained(&data, true, Duration::ZERO, &r).unwrap();
    assert_eq!(dry.kept_busy, vec![busy.clone()]);
    let real = gc::collect_retained(&data, false, Duration::ZERO, &r).unwrap();
    assert!(real.expired.is_empty(), "{real:?}");
    assert_eq!(real.kept_busy, vec![busy.clone()]);
    assert!(exists(&data, &busy));
}

#[test]
fn an_unreadable_record_stops_the_collection_before_anything_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let a = closed(&data, tmp.path(), "a", now_ms() - 9 * DAY_MS);
    let bad = closed(&data, tmp.path(), "bad", now_ms() - 8 * DAY_MS);
    std::fs::write(data.session(&bad).state(), b"{not json").unwrap();
    let err = gc::collect_retained(&data, false, Duration::ZERO, &keep(None, Some(0)))
        .unwrap_err()
        .to_string();
    assert!(err.contains("record cannot be read"), "{err}");
    assert!(exists(&data, &a), "nothing was moved");
    assert!(!data.root().join(".trash").exists());
}
