#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd gc --dry-run`: the roots come from the session records and the
//! ledgers, an unreadable session stops the report, and nothing is deleted
//! (#296).

use std::collections::BTreeMap;

use wyrd_cas::{Cas, Manifest};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::state::{self, CheckpointRecord};
use wyrd_cell::{DataDir, Session, gc};

fn session(data: &DataDir, ws: &std::path::Path) -> Session {
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

/// Two sessions with one checkpoint each, plus one chunk nothing refers to.
fn store() -> (tempfile::TempDir, DataDir, Vec<Session>) {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut sessions = Vec::new();
    for i in 0..2 {
        let mut s = session(&data, &tmp.path().join(format!("ws{i}")));
        std::fs::write(s.dir().upper("a").join("f.txt"), format!("content {i}\n")).unwrap();
        s.checkpoint(None).unwrap();
        sessions.push(s);
    }
    Cas::open(data.cas())
        .unwrap()
        .put(b"nobody refers to this")
        .unwrap();
    (tmp, data, sessions)
}

fn chunk_files(data: &DataDir) -> usize {
    walk(&data.cas().join("chunks"))
}

fn walk(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() { walk(&p) } else { 1 }
            })
            .sum()
    })
}

#[test]
fn a_dry_run_reports_the_unreferenced_chunk_and_deletes_nothing() {
    let (_tmp, data, _s) = store();
    let before = chunk_files(&data);
    let r = gc::dry_run(&data).unwrap();
    assert_eq!(r.deleted, 1, "{r:?}");
    assert_eq!(r.kept + r.deleted, before);
    assert!(r.bytes_freed > 0);
    assert_eq!(chunk_files(&data), before, "a dry run deletes nothing");
    assert_eq!(gc::dry_run(&data).unwrap().deleted, 1);
    assert_eq!(gc::roots(&data).unwrap().len(), 2);
}

#[test]
fn a_checkpoint_named_only_in_the_ledger_keeps_its_chunks() {
    let (_tmp, data, s) = store();
    // The record loses its checkpoints (as after a crash between the event
    // and the state save); the ledger still names them.
    let mut rec = state::load(s[0].dir()).unwrap();
    rec.checkpoints.clear();
    state::save(s[0].dir(), &rec).unwrap();
    assert_eq!(gc::dry_run(&data).unwrap().deleted, 1);
}

#[test]
fn a_checkpoint_named_only_in_the_record_keeps_its_manifest() {
    let (_tmp, data, s) = store();
    let cas = Cas::open(data.cas()).unwrap();
    let orphan = cas.put_manifest(&Manifest::default()).unwrap();
    assert_eq!(
        gc::dry_run(&data).unwrap().deleted,
        2,
        "the manifest is unreferenced"
    );
    let mut rec = state::load(s[1].dir()).unwrap();
    let mut extra: CheckpointRecord = rec.checkpoints[0].clone();
    extra.id = "c99".into();
    extra.manifest = orphan.to_string();
    rec.checkpoints.push(extra);
    state::save(s[1].dir(), &rec).unwrap();
    assert_eq!(gc::dry_run(&data).unwrap().deleted, 1);
}

#[test]
fn an_unreadable_record_or_ledger_stops_the_report() {
    let (_tmp, data, s) = store();
    let state_file = s[0].dir().state();
    let saved = std::fs::read(&state_file).unwrap();
    std::fs::write(&state_file, b"{not json").unwrap();
    let e = gc::dry_run(&data).unwrap_err().to_string();
    assert!(e.contains("record cannot be read"), "{e}");
    std::fs::write(&state_file, saved).unwrap();

    let ledger = s[1].dir().ledger();
    for suffix in ["-wal", "-shm"] {
        let mut p = ledger.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    std::fs::write(&ledger, b"not a database").unwrap();
    let e = gc::dry_run(&data).unwrap_err().to_string();
    assert!(e.contains("ledger cannot be read"), "{e}");
}

#[test]
fn an_empty_data_directory_has_no_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    assert!(gc::roots(&data).unwrap().is_empty());
}
