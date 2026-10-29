#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A rewind does not sync its restored tree; instead a branch that was not
//! restored in this boot is restored again before anything uses it. These tests
//! plant the states a crash can leave and check the recovery. A power cut is
//! not simulated: the states are written by hand.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wyrd_cas::{Cas, ChunkHash, Step, record_steps};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

fn create(tmp: &tempfile::TempDir) -> (DataDir, Session) {
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
    fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
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
    .unwrap();
    (data, s)
}

/// Two checkpoints: c1 has a.txt; c2 adds b.txt.
fn two_checkpoints(tmp: &tempfile::TempDir) -> (DataDir, Session) {
    let (data, mut s) = create(tmp);
    s.checkpoint(None).unwrap();
    fs::write(s.dir().upper("a").join("b.txt"), "b\n").unwrap();
    s.checkpoint(None).unwrap();
    (data, s)
}

/// Every regular file under `dir`, by relative path.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The tree a manifest describes, materialised into a scratch directory.
fn expected(
    data: &DataDir,
    manifest_hash: &str,
    tmp: &tempfile::TempDir,
) -> BTreeMap<PathBuf, Vec<u8>> {
    let cas = Cas::open(data.cas()).unwrap();
    let m = cas
        .get_manifest(ChunkHash::from_hex(manifest_hash).unwrap())
        .unwrap();
    let dest = tmp.path().join("expected");
    cas.materialize(&m, &dest).unwrap();
    tree(&dest)
}

fn position(steps: &[Step], pred: impl Fn(&Step) -> bool) -> Option<usize> {
    steps.iter().position(pred)
}

#[test]
fn a_rewind_records_its_branch_and_does_not_sync_the_restored_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, mut s) = two_checkpoints(&tmp);
    let dir = data.session(&s.record().id);
    let (res, steps) = record_steps(|| s.rewind("c1", None));
    res.unwrap();
    // No barrier: the tree is not synced, the branch is.
    assert!(
        !steps.contains(&Step::SyncFs),
        "a rewind makes no syncfs: {steps:?}"
    );
    let branch_record = position(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if *to == dir.state()),
    );
    assert!(branch_record.is_some(), "the branch is recorded: {steps:?}");
}

#[test]
fn an_interrupted_restore_is_redone_when_the_session_is_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, mut s) = two_checkpoints(&tmp);
    let id = s.record().id.clone();
    let c1 = s.record().checkpoints[0].manifest.clone();
    s.rewind("c1", None).unwrap();
    let branch = s.record().current_branch.clone();
    drop(s);

    // A crash mid-restore: the marker is gone and the tree is half-written.
    let branch_dir = data.session(&id).branch(&branch);
    fs::remove_file(branch_dir.join("restored")).unwrap();
    fs::write(branch_dir.join("upper").join("junk.txt"), "half").unwrap();
    fs::write(branch_dir.join("upper").join("a.txt"), "tr").unwrap();

    let _reopened = Session::open(&data, &id).unwrap();
    assert_eq!(
        tree(&branch_dir.join("upper")),
        expected(&data, &c1, &tmp),
        "redone from the content store"
    );
}

#[test]
fn a_checkpoint_never_snapshots_a_half_restored_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, mut s) = two_checkpoints(&tmp);
    let id = s.record().id.clone();
    let c1 = s.record().checkpoints[0].manifest.clone();
    s.rewind("c1", None).unwrap();
    let branch = s.record().current_branch.clone();
    let branch_dir = data.session(&id).branch(&branch);
    fs::remove_file(branch_dir.join("restored")).unwrap();
    fs::write(branch_dir.join("upper").join("junk.txt"), "half").unwrap();

    let cp = s.checkpoint(None).unwrap();
    assert_eq!(
        cp.manifest, c1,
        "the checkpoint records the restored tree, not the junk"
    );
    let cas = Cas::open(data.cas()).unwrap();
    let m = cas
        .get_manifest(ChunkHash::from_hex(&cp.manifest).unwrap())
        .unwrap();
    assert!(m.entries.iter().all(|e| !e.path.ends_with(b"junk.txt")));
}

#[test]
fn a_restore_confirmed_in_this_boot_is_not_redone_on_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, mut s) = two_checkpoints(&tmp);
    let id = s.record().id.clone();
    s.rewind("c1", None).unwrap();
    let branch = s.record().current_branch.clone();
    drop(s);
    let upper = data.session(&id).branch(&branch).join("upper");
    fs::write(upper.join("kept.txt"), "marker says restored").unwrap();

    let _reopened = Session::open(&data, &id).unwrap();
    assert!(
        upper.join("kept.txt").exists(),
        "a confirmed restore is trusted, not redone"
    );
}

#[test]
fn a_restore_marked_in_another_boot_is_redone_on_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, mut s) = two_checkpoints(&tmp);
    let id = s.record().id.clone();
    s.rewind("c1", None).unwrap();
    let branch = s.record().current_branch.clone();
    drop(s);
    let branch_dir = data.session(&id).branch(&branch);
    fs::write(branch_dir.join("restored"), "a-boot-that-is-over").unwrap();
    fs::write(
        branch_dir.join("upper").join("stale.txt"),
        "from before the reboot",
    )
    .unwrap();

    let _reopened = Session::open(&data, &id).unwrap();
    assert!(
        !branch_dir.join("upper").join("stale.txt").exists(),
        "the old boot's tree is rebuilt"
    );
}
