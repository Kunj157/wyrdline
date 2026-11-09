#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd backup` (#318): a consistent copy that verifies, refuses to copy a
//! running or busy session, leaves the host private key out unless asked, and
//! never leaves a partial copy under the destination name.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use wyrd_cas::Cas;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::state;
use wyrd_cell::{CellError, DataDir, Session, backup};
use wyrd_schema::SessionState;

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

/// Two sessions with a checkpoint each; the first is closed and sealed, so
/// the host key exists. Returns the data dir and the closed session's head.
fn store(root: &Path) -> (DataDir, String, Vec<String>) {
    let data = DataDir::new(root.join("data"));
    let mut ids = Vec::new();
    let mut heads = String::new();
    for i in 0..2 {
        let mut s = session(&data, &root.join(format!("ws{i}")));
        std::fs::write(s.dir().upper("a").join("f.txt"), format!("content {i}\n")).unwrap();
        s.checkpoint(None).unwrap();
        if i == 0 {
            s.close("done").unwrap();
            heads = wyrd_ledger::verify(&s.dir().ledger()).unwrap().head_hash;
        }
        ids.push(s.record().id.clone());
    }
    (data, heads, ids)
}

fn dest(root: &Path) -> std::path::PathBuf {
    root.join("out").join("backup")
}

#[test]
fn a_backup_verifies_and_holds_the_sessions_and_their_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, head, ids) = store(tmp.path());
    let m = backup::backup(&data, &dest(tmp.path()), false).unwrap();
    assert_eq!(m.format, backup::FORMAT_ID);
    assert_eq!(m.sessions.len(), 2);
    assert_eq!(m.roots, 2, "one checkpoint per session");
    assert!(m.chunks >= 2);
    let closed = m.sessions.iter().find(|s| s.id == ids[0]).unwrap();
    assert!(closed.sealed);
    assert_eq!(closed.status, SessionState::Closed);
    assert_eq!(closed.head_hash, head, "the copy's head is the original's");
    assert!(m.host_public_key.is_some());
    assert!(!m.host_key_included);

    // The copy is itself a verifiable tree, and the manifest is on disk.
    let again = backup::verify_tree(&dest(tmp.path())).unwrap();
    assert_eq!(again.sessions, m.sessions);
    assert_eq!(backup::read_manifest(&dest(tmp.path())).unwrap(), m);
}

#[test]
fn the_private_key_is_left_out_unless_asked_and_stays_private() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, _) = store(tmp.path());
    let without = tmp.path().join("without");
    let m = backup::backup(&data, &without, false).unwrap();
    assert!(!without.join("keys/host.key").exists());
    assert!(m.host_fingerprint.is_some(), "the public half is recorded");

    let with = tmp.path().join("with");
    let m = backup::backup(&data, &with, true).unwrap();
    assert!(m.host_key_included);
    let mode_of = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_of(&with.join("keys/host.key")), 0o600);
    assert_eq!(mode_of(&with.join("keys")), 0o700);
    assert_eq!(mode_of(&with), 0o700);
}

#[test]
fn a_running_session_refuses_the_backup_and_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, ids) = store(tmp.path());
    // A populated cgroup stands for a live Cell; the record points at it.
    let cg = tmp.path().join("cgroup");
    std::fs::create_dir_all(&cg).unwrap();
    std::fs::write(cg.join("cgroup.procs"), "").unwrap();
    std::fs::write(cg.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    let s = data.session(&ids[1]);
    let mut rec = state::load(&s).unwrap();
    rec.cgroup = Some(cg);
    state::save(&s, &rec).unwrap();

    let err = backup::backup(&data, &dest(tmp.path()), false).unwrap_err();
    let CellError::Busy(msg) = &err else {
        panic!("expected a busy error, got {err}")
    };
    assert!(msg.contains(&ids[1]), "{msg}");
    assert!(!dest(tmp.path()).exists(), "nothing is left under the name");
    assert!(!tmp.path().join("out").read_dir().unwrap().any(|_| true));
}

#[test]
fn an_import_in_progress_refuses_the_backup_and_leaves_nothing_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, _) = store(tmp.path());
    std::fs::create_dir_all(data.sessions().join(".import-x")).unwrap();
    let parent = tmp.path().join("out");
    let err = backup::backup(&data, &dest(tmp.path()), false).unwrap_err();
    assert!(matches!(err, CellError::Busy(_)), "{err}");
    assert!(
        !parent.exists() || parent.read_dir().unwrap().next().is_none(),
        "no staging directory is left beside the destination"
    );
}

#[test]
fn a_session_whose_recorded_pid_is_alive_refuses_and_a_dead_one_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, ids) = store(tmp.path());
    let s = data.session(&ids[1]);
    let mut rec = state::load(&s).unwrap();
    // This process is alive: observe-mode and non-root runs record no cgroup,
    // so the pid is what marks them running.
    rec.pid = Some(std::process::id());
    state::save(&s, &rec).unwrap();
    let err = backup::backup(&data, &dest(tmp.path()), false).unwrap_err();
    assert!(
        matches!(err, CellError::Busy(ref m) if m.contains(&ids[1])),
        "{err}"
    );

    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    rec.pid = Some(dead);
    state::save(&s, &rec).unwrap();
    backup::backup(&data, &dest(tmp.path()), false).unwrap();
}

#[test]
fn a_session_whose_lock_is_held_refuses_the_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, ids) = store(tmp.path());
    let _held = state::lock(&data.session(&ids[0]).lock()).unwrap();
    let err = backup::backup(&data, &dest(tmp.path()), false).unwrap_err();
    assert!(matches!(err, CellError::Busy(_)), "{err}");
    assert!(!dest(tmp.path()).exists());
}

#[test]
fn a_destination_that_is_not_empty_or_inside_the_data_dir_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, _) = store(tmp.path());
    let occupied = tmp.path().join("occupied");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("keep.txt"), "mine").unwrap();
    let err = backup::backup(&data, &occupied, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not an empty directory"), "{err}");
    assert_eq!(std::fs::read(occupied.join("keep.txt")).unwrap(), b"mine");

    let inside = data.root().join("backup-here");
    let err = backup::backup(&data, &inside, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("must not contain each other"), "{err}");
    assert!(!inside.exists());

    // An empty directory is an acceptable destination.
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    backup::backup(&data, &empty, false).unwrap();
}

#[test]
fn a_corrupt_chunk_fails_the_backup_before_anything_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, _, _) = store(tmp.path());
    let cas = Cas::open(data.cas()).unwrap();
    let root = wyrd_cell::gc::roots(&data)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let manifest = cas.get_manifest(root).unwrap();
    let chunk = manifest.entries[0].chunks[0];
    let hex = format!("{chunk}");
    let hex = hex.trim_start_matches("b3:");
    let path = data
        .cas()
        .join("chunks")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(hex);
    std::fs::write(&path, b"damaged").unwrap();
    assert!(backup::backup(&data, &dest(tmp.path()), false).is_err());
    assert!(!dest(tmp.path()).exists());
}
