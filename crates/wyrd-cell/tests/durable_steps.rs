#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The order of the durable steps of session creation, rewind and import. A
//! new directory is durable in its parent before a file is renamed into it,
//! a file is durable before it is renamed into place, and the rewind's
//! restore is durable before its branch change. A power cut is not simulated.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wyrd_cas::{Step, record_steps};
use wyrd_cell::session::{CreateOptions, ImportSource, ImportSpec};
use wyrd_cell::{DataDir, Session};
use wyrd_schema::Event;
use wyrd_schema::{CaptureMode, EventBody, SessionStarted};

fn parent(p: &Path) -> PathBuf {
    p.parent().unwrap().to_path_buf()
}

/// A sync that makes `dir`'s entries durable: of `dir`, or a `syncfs`.
fn durable_after(steps: &[Step], from: usize, dir: &Path) -> Option<usize> {
    steps[from..]
        .iter()
        .position(|t| *t == Step::SyncDir(dir.to_path_buf()) || *t == Step::SyncFs)
        .map(|k| from + k)
}

/// Every created directory is durable in its parent before a file is renamed
/// into it, and every rename is followed by a durable sync of its directory.
fn check_invariant(steps: &[Step]) {
    for (i, s) in steps.iter().enumerate() {
        match s {
            Step::Mkdir(d) => {
                let synced = durable_after(steps, i + 1, &parent(d)).unwrap_or_else(|| {
                    panic!(
                        "{} was made, its parent is never synced: {steps:?}",
                        d.display()
                    )
                });
                let renamed_before = steps[i + 1..synced]
                    .iter()
                    .any(|t| matches!(t, Step::Rename(_, to) if parent(to) == *d));
                assert!(
                    !renamed_before,
                    "a file was renamed into {} first: {steps:?}",
                    d.display()
                );
            }
            Step::Rename(_, to) => {
                assert!(
                    durable_after(steps, i + 1, &parent(to)).is_some(),
                    "rename into {} is never synced: {steps:?}",
                    parent(to).display()
                );
            }
            _ => {}
        }
    }
}

fn position(steps: &[Step], pred: impl Fn(&Step) -> bool) -> usize {
    steps
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("missing step in {steps:?}"))
}

fn create_opts(workspace: PathBuf) -> CreateOptions {
    CreateOptions {
        workspace,
        command: vec!["true".into()],
        observe: false,
        name: None,
        agent_kind: "test".into(),
        deny_tools: Vec::new(),
        policy: None,
        kernel_release: "test".into(),
        capabilities: BTreeMap::new(),
    }
}

#[test]
fn creating_a_session_makes_each_directory_and_file_durable_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));

    let (res, steps) = record_steps(|| Session::create(&data, create_opts(ws.clone())));
    let s = res.unwrap();
    check_invariant(&steps);
    let dir = data.session(&s.record().id);

    // The data and sessions directories, and the session directory, each
    // synced into its parent once made.
    assert!(
        steps.contains(&Step::SyncDir(data.root().to_path_buf())),
        "{steps:?}"
    );
    assert!(steps.contains(&Step::SyncDir(data.sessions())), "{steps:?}");
    assert!(
        steps.contains(&Step::SyncDir(dir.root().to_path_buf())),
        "{steps:?}"
    );

    // The session record is renamed into place, and then the session
    // directory is synced: that sync also makes the ledger's new name durable.
    let state_rename = position(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == dir.state()),
    );
    assert!(
        steps[state_rename..].contains(&Step::SyncDir(dir.root().to_path_buf())),
        "the session directory is synced after the record is renamed in: {steps:?}"
    );
    // The baseline is durable too.
    let baseline = position(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == dir.baseline()),
    );
    assert!(
        steps[baseline..].contains(&Step::SyncDir(dir.root().to_path_buf())),
        "{steps:?}"
    );
}

#[test]
fn a_rewind_restores_and_syncs_the_tree_before_it_changes_the_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(&data, create_opts(ws.clone())).unwrap();
    let dir = data.session(&s.record().id);
    s.checkpoint(None).unwrap();
    std::fs::write(s.dir().upper("a").join("b.txt"), "new\n").unwrap();
    s.checkpoint(None).unwrap();

    let (res, steps) = record_steps(|| s.rewind("c1", None));
    res.unwrap();
    check_invariant(&steps);
    let synced_tree = position(&steps, |t| *t == Step::SyncFs);
    let branch_change = position(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == dir.state()),
    );
    assert!(
        synced_tree < branch_change,
        "the restored tree is synced before the record that names its branch: {steps:?}"
    );
}

#[test]
fn an_import_syncs_its_ledger_and_files_before_the_session_becomes_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let spec = ImportSpec {
        name: None,
        agent: "test".into(),
        mode: CaptureMode::Observe,
        created_ms: 1,
        events: vec![Event {
            seq: 1,
            ts_mono_ns: 1,
            ts_wall_ms: 1,
            process: None,
            drops_since_last: 0,
            body: EventBody::SessionStarted(SessionStarted {
                session_id: "orig".into(),
                agent_kind: "test".into(),
                agent_version: None,
                command: vec!["true".into()],
                workspace_root: "/w".into(),
                mode: CaptureMode::Observe,
                kernel_release: "test".into(),
                capabilities: BTreeMap::new(),
                policy_set_hash: None,
            }),
        }],
        chunks: Vec::new(),
        source: ImportSource {
            original_id: "orig".into(),
            manifest_hash: "b3:00".into(),
            signer_fingerprint: "fp".into(),
            redaction: "standard".into(),
            content: false,
            imported_ms: 2,
        },
    };
    let (res, steps) = record_steps(|| Session::import(&data, &spec));
    let s = res.unwrap();
    check_invariant(&steps);
    let visible = data.session(&s.record().id);
    // The ledger's events are on disk before the rename that makes the session visible.
    let ledger_sync = position(
        &steps,
        |t| matches!(t, Step::SyncFile(p) if p.ends_with("ledger.sqlite") || p.to_string_lossy().contains("ledger")),
    );
    let make_visible = position(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == visible.root()),
    );
    assert!(ledger_sync < make_visible, "{steps:?}");
    // The new name is in `sessions/`, which is synced after the rename.
    assert!(
        steps[make_visible..].contains(&Step::SyncDir(data.sessions())),
        "{steps:?}"
    );
}
