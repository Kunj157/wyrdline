#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A deterministic guard on the checkpoint's durability work, for the PR gate.
//! Timing on a shared runner is too noisy to gate on, so this counts the
//! durable steps instead: a checkpoint makes exactly two `syncfs` barriers,
//! and no per-file or per-directory sync in the content store. The steps are
//! recorded process-wide, so the snapshot's worker threads are included.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wyrd_cas::{Step, record_steps};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

fn session(tmp: &tempfile::TempDir) -> Session {
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    Session::create(
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
    .unwrap()
}

/// Bytes unique to `file`: distinct content, several chunks each.
fn body(file: u32) -> Vec<u8> {
    (0..200_000u32)
        .map(|j| (j.wrapping_mul(2_654_435_761) ^ file.wrapping_mul(40_503)).to_le_bytes()[0])
        .collect()
}

/// A path in the content store (`cas/`), as opposed to the session record.
fn in_cas(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str() == "cas")
}

fn count(steps: &[Step], pred: impl Fn(&Step) -> bool) -> usize {
    steps.iter().filter(|t| pred(t)).count()
}

fn positions(steps: &[Step], pred: impl Fn(&Step) -> bool) -> Vec<usize> {
    steps
        .iter()
        .enumerate()
        .filter(|(_, t)| pred(t))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn a_checkpoint_of_fifty_changed_files_makes_exactly_two_barriers_and_no_per_file_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = session(&tmp);
    for f in 0..50u32 {
        std::fs::write(s.dir().upper("a").join(format!("f{f:02}.bin")), body(f)).unwrap();
    }

    let (res, steps) = record_steps(|| s.checkpoint(None));
    res.unwrap();

    let barriers = positions(&steps, |t| *t == Step::SyncFs);
    assert_eq!(barriers.len(), 2, "exactly two syncfs barriers: {steps:?}");
    assert_eq!(
        count(&steps, |t| matches!(t, Step::SyncFile(p) if in_cas(p))),
        0,
        "no per-chunk data sync: {steps:?}"
    );
    assert_eq!(
        count(&steps, |t| matches!(t, Step::SyncDir(p) if in_cas(p))),
        0,
        "no per-directory sync in the content store: {steps:?}"
    );

    // Every changed file was chunked: at least one rename per file, into more
    // than one chunk directory.
    let renamed: Vec<PathBuf> = steps
        .iter()
        .filter_map(|t| match t {
            Step::Rename(_, to) if in_cas(to) => Some(to.parent().unwrap().to_path_buf()),
            _ => None,
        })
        .collect();
    assert!(
        renamed.len() >= 50,
        "each file has at least one chunk: {}",
        renamed.len()
    );
    let mut dirs = renamed.clone();
    dirs.sort();
    dirs.dedup();
    assert!(
        dirs.len() > 1,
        "the chunks are spread over several directories"
    );

    // Order: every content-store rename sits between the two barriers, and the
    // ledger event comes after the second.
    let (first, second) = (barriers[0], barriers[1]);
    for r in positions(&steps, |t| matches!(t, Step::Rename(_, to) if in_cas(to))) {
        assert!(
            first < r && r < second,
            "a rename outside the barriers: {steps:?}"
        );
    }
    let event = positions(&steps, |t| *t == Step::LedgerAppend);
    assert_eq!(event.len(), 1, "one checkpoint event: {steps:?}");
    assert!(
        second < event[0],
        "the event is written after the second barrier: {steps:?}"
    );
}

#[test]
fn a_checkpoint_with_no_changes_pins_its_current_barriers_and_event() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = session(&tmp);
    std::fs::write(s.dir().upper("a").join("only.bin"), body(999)).unwrap();
    s.checkpoint(None).unwrap();

    // Nothing changed since: the manifest is the same, so no chunk or manifest
    // is written again. The two barriers and the checkpoint event still happen.
    let (res, steps) = record_steps(|| s.checkpoint(None));
    res.unwrap();
    assert_eq!(count(&steps, |t| *t == Step::SyncFs), 2, "{steps:?}");
    assert_eq!(
        count(&steps, |t| matches!(t, Step::Rename(_, to) if in_cas(to))),
        0,
        "nothing rewritten in the content store: {steps:?}"
    );
    assert_eq!(count(&steps, |t| *t == Step::LedgerAppend), 1, "{steps:?}");
    // The session record is written after the event: the checkpoint's status update.
    let event = positions(&steps, |t| *t == Step::LedgerAppend)[0];
    let record = positions(
        &steps,
        |t| matches!(t, Step::Rename(_, to) if to.ends_with("session.json")),
    );
    assert!(
        record.iter().all(|r| *r > event),
        "the record follows the event: {steps:?}"
    );
}
