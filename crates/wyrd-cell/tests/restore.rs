#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd restore` (#318): a backup restores into an absent or empty data
//! directory, checked against its manifest. A restored session opens and keeps
//! its checkpoints. Anything else refuses without touching the target.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{CellError, DataDir, Session, backup};

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

/// Two sessions. The first has two checkpoints and is closed (so the host key
/// exists); the second has one.
fn store(root: &Path) -> (DataDir, String) {
    let (data, a, _) = store_with_open(root);
    (data, a)
}

/// As [`store`], also returning the id of the second session, which is open
/// (its checkpoint has edits nobody has checked out since).
fn store_with_open(root: &Path) -> (DataDir, String, String) {
    let data = DataDir::new(root.join("data"));
    let mut a = session(&data, &root.join("wsa"));
    std::fs::write(a.dir().upper("a").join("f.txt"), "one\n").unwrap();
    a.checkpoint(None).unwrap();
    std::fs::write(a.dir().upper("a").join("g.txt"), "two\n").unwrap();
    a.checkpoint(None).unwrap();
    a.close("done").unwrap();
    let id = a.record().id.clone();
    let mut b = session(&data, &root.join("wsb"));
    std::fs::write(b.dir().upper("a").join("h.txt"), "three\n").unwrap();
    b.checkpoint(None).unwrap();
    let b_id = b.record().id.clone();
    (data, id, b_id)
}

#[test]
fn an_open_session_checkpoints_on_from_its_restored_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, open_id) = store_with_open(tmp.path());
    let last = wyrd_cell::state::load(&data.session(&open_id))
        .unwrap()
        .checkpoints
        .last()
        .unwrap()
        .clone();
    let dir = backup_of(&data, tmp.path(), false);
    let target = DataDir::new(tmp.path().join("restored/data"));
    backup::restore(&dir, &target).unwrap();

    // No edits since the restore: a new checkpoint must match the old head,
    // not an empty tree (which would show the file as deleted).
    let mut open = Session::open(&target, &open_id).unwrap();
    let cp = open.checkpoint(None).unwrap();
    assert_eq!(
        cp.manifest, last.manifest,
        "the restored branch state is the head"
    );
    let changes = open.diff(&last.id, &cp.id).unwrap();
    assert!(changes.is_empty(), "{changes:?}");
}

fn backup_of(data: &DataDir, root: &Path, include_key: bool) -> PathBuf {
    let dir = root.join("backup");
    backup::backup(data, &dir, include_key).unwrap();
    dir
}

#[test]
fn a_backup_restores_verified_and_its_sessions_open_with_their_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, id) = store(tmp.path());
    let before = backup::verify_tree(data.root()).unwrap();
    let dir = backup_of(&data, tmp.path(), false);

    let target = DataDir::new(tmp.path().join("restored/data"));
    let m = backup::restore(&dir, &target).unwrap();
    assert_eq!(m.sessions, before.sessions);
    let again = backup::verify_tree(target.root()).unwrap();
    assert_eq!(again.sessions, before.sessions);
    assert_eq!(again.roots, before.roots);
    assert_eq!(again.chunks, before.chunks);

    let restored = Session::open(&target, &id).unwrap();
    assert_eq!(restored.record().checkpoints.len(), 2);
    let changes = restored.diff("c1", "c2").unwrap();
    assert!(
        changes.iter().any(|c| format!("{c:?}").contains("g.txt")),
        "{changes:?}"
    );
    let head = |d: &DataDir| {
        wyrd_ledger::verify(&d.session(&id).ledger())
            .unwrap()
            .head_hash
    };
    assert_eq!(
        head(&target),
        head(&data),
        "the restored head is the original head"
    );
    assert_eq!(Session::list(&target).unwrap().len(), 2);
}

#[test]
fn a_restore_refuses_a_non_empty_target_and_leaves_it_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _) = store(tmp.path());
    let dir = backup_of(&data, tmp.path(), false);
    let target = DataDir::new(tmp.path().join("occupied"));
    std::fs::create_dir_all(target.root()).unwrap();
    std::fs::write(target.root().join("mine.txt"), "keep").unwrap();
    let err = backup::restore(&dir, &target).unwrap_err();
    assert!(matches!(err, CellError::Occupied(_)), "{err}");
    assert_eq!(
        std::fs::read(target.root().join("mine.txt")).unwrap(),
        b"keep"
    );
    assert!(!target.sessions().exists());
}

#[test]
fn a_backup_that_does_not_match_its_manifest_restores_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _) = store(tmp.path());
    let dir = backup_of(&data, tmp.path(), false);
    let manifest_path = dir.join(backup::MANIFEST);
    let mut m = backup::read_manifest(&dir).unwrap();
    m.chunks += 1;
    std::fs::write(&manifest_path, serde_json::to_vec(&m).unwrap()).unwrap();

    let target = DataDir::new(tmp.path().join("restored/data"));
    let err = backup::restore(&dir, &target).unwrap_err();
    assert!(
        err.to_string().contains("does not match its manifest"),
        "{err}"
    );
    assert!(!target.root().exists(), "the target is not created");
    assert!(
        !tmp.path()
            .join("restored")
            .read_dir()
            .unwrap()
            .any(|_| true),
        "no staging directory is left beside it"
    );
}

#[test]
fn a_backup_with_an_unexpected_entry_or_a_corrupt_chunk_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _) = store(tmp.path());
    let dir = backup_of(&data, tmp.path(), false);

    std::fs::write(dir.join("planted.sh"), "#!/bin/sh\n").unwrap();
    let target = DataDir::new(tmp.path().join("t1/data"));
    let err = backup::restore(&dir, &target).unwrap_err();
    assert!(
        err.to_string().contains("unexpected entry planted.sh"),
        "{err}"
    );
    std::fs::remove_file(dir.join("planted.sh")).unwrap();

    let root = wyrd_cell::gc::roots(&DataDir::new(&dir))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let cas = wyrd_cas::Cas::open(dir.join("cas")).unwrap();
    let chunk = cas.get_manifest(root).unwrap().entries[0].chunks[0];
    let hex = format!("{chunk}");
    let hex = hex.trim_start_matches("b3:");
    let path = dir
        .join("cas/chunks")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(hex);
    std::fs::write(&path, b"damaged").unwrap();
    let target = DataDir::new(tmp.path().join("t2/data"));
    assert!(backup::restore(&dir, &target).is_err());
    assert!(!target.root().exists());
}

#[test]
fn the_host_key_is_restored_only_when_the_backup_has_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _) = store(tmp.path());
    let plain = backup_of(&data, &tmp.path().join("p"), false);
    let t1 = DataDir::new(tmp.path().join("r1/data"));
    backup::restore(&plain, &t1).unwrap();
    assert!(!t1.host_key().exists());

    let keyed = backup_of(&data, &tmp.path().join("k"), true);
    let t2 = DataDir::new(tmp.path().join("r2/data"));
    backup::restore(&keyed, &t2).unwrap();
    assert_eq!(
        std::fs::metadata(t2.host_key())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        wyrd_ledger::HostKey::load(&t2.host_key())
            .unwrap()
            .public_key(),
        wyrd_ledger::HostKey::load(&data.host_key())
            .unwrap()
            .public_key()
    );
}

#[test]
fn a_backup_may_not_hold_entries_outside_what_backup_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, id) = store(tmp.path());
    let dir = backup_of(&data, tmp.path(), false);
    let refused = |dir: &Path, name: &str| {
        let target = DataDir::new(tmp.path().join(name).join("data"));
        let err = backup::restore(dir, &target).unwrap_err().to_string();
        assert!(err.contains("unexpected entry"), "{err}");
        assert!(!target.root().exists(), "{name}: the target is not created");
    };

    // A dot-entry under sessions/ would make every later backup refuse.
    let dot = dir.join("sessions/.import-x");
    std::fs::create_dir_all(&dot).unwrap();
    refused(&dir, "dot");
    std::fs::remove_dir(&dot).unwrap();

    // A file that backup never writes, inside a session directory.
    let extra = dir.join(format!("sessions/{id}/planted.sh"));
    std::fs::write(&extra, "#!/bin/sh\n").unwrap();
    refused(&dir, "planted");
    std::fs::remove_file(&extra).unwrap();

    // A key other than host.key.
    std::fs::create_dir_all(dir.join("keys")).unwrap();
    std::fs::write(dir.join("keys/other.key"), "x").unwrap();
    refused(&dir, "key");
    std::fs::remove_file(dir.join("keys/other.key")).unwrap();

    // Still restores once the extras are gone.
    backup::restore(&dir, &DataDir::new(tmp.path().join("ok/data"))).unwrap();
}
