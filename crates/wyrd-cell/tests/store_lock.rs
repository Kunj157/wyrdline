#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The store lock around a checkpoint and an import (#296). A writer holds the
//! store lock shared from before its first chunk write until its reference to
//! those chunks is durable, so `gc` cannot sweep them in between. The order is
//! pinned by the recorded steps, not by timing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wyrd_cas::{Step, record_steps};
use wyrd_cell::session::{CreateOptions, ImportSource, ImportSpec};
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{CaptureMode, Event, EventBody, SessionStarted};

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

fn first(steps: &[Step], pred: impl Fn(&Step) -> bool) -> usize {
    steps
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("missing step in {steps:?}"))
}

/// The first step at or after `from` that is `Unlock`.
fn unlock_after(steps: &[Step], from: usize) -> usize {
    from + first(&steps[from..], |t| *t == Step::Unlock)
}

/// A step that writes into the content store (`cas`): a directory made or
/// synced there, or a file renamed into or out of it.
fn touches(cas: &Path, t: &Step) -> bool {
    match t {
        Step::Mkdir(p) | Step::SyncDir(p) | Step::SyncFile(p) => p.starts_with(cas),
        Step::Rename(from, to) => from.starts_with(cas) || to.starts_with(cas),
        _ => false,
    }
}

/// A chunk's staging file is renamed out of the store's `tmp/`, so this step
/// is the chunk write itself, whether a checkpoint's batch or a `put`.
fn chunk_write(cas: &Path) -> impl Fn(&Step) -> bool + '_ {
    move |t| matches!(t, Step::Rename(from, _) if from.starts_with(cas.join("tmp")))
}

#[test]
fn a_checkpoint_holds_the_store_lock_from_its_first_chunk_to_its_state_save() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(&data, create_opts(ws)).unwrap();
    std::fs::write(s.dir().upper("a").join("f.txt"), b"checkpointed\n").unwrap();
    let cas = data.cas();
    let dir = s.dir().clone();

    let (res, steps) = record_steps(|| s.checkpoint(None));
    res.unwrap();

    let lock = first(&steps, |t| *t == Step::Lock { exclusive: false });
    let unlock = unlock_after(&steps, lock);
    assert_eq!(
        steps
            .iter()
            .filter(|t| matches!(t, Step::Lock { .. }))
            .count(),
        1,
        "{steps:?}"
    );
    let first_write = first(&steps, |t| touches(&cas, t));
    let chunk = first(&steps, chunk_write(&cas));
    let event = first(&steps, |t| *t == Step::LedgerAppend);
    let state = first(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == dir.state()),
    );
    assert!(
        lock < first_write && first_write <= chunk && chunk < event && event < state,
        "{steps:?}"
    );
    assert!(
        state < unlock,
        "the lock was released before session.json was saved: {steps:?}"
    );
}

#[test]
fn an_import_holds_the_store_lock_until_its_session_is_visible() {
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
        chunks: vec![b"imported chunk".to_vec()],
        source: ImportSource {
            original_id: "orig".into(),
            manifest_hash: "b3:00".into(),
            signer_fingerprint: "fp".into(),
            redaction: "standard".into(),
            content: true,
            imported_ms: 2,
        },
    };
    let cas = data.cas();
    let (res, steps) = record_steps(|| Session::import(&data, &spec));
    let s = res.unwrap();
    let visible = data.session(&s.record().id).root().to_path_buf();

    let lock = first(&steps, |t| *t == Step::Lock { exclusive: false });
    let unlock = unlock_after(&steps, lock);
    let chunk = first(&steps, chunk_write(&cas));
    let make_visible = first(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == visible),
    );
    let sessions_synced = make_visible
        + first(&steps[make_visible..], |t| {
            *t == Step::SyncDir(data.sessions())
        });
    assert!(
        lock < chunk && chunk < make_visible && sessions_synced < unlock,
        "{steps:?}"
    );
}
