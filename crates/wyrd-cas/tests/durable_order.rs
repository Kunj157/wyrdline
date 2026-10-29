#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

//! Every durable write syncs a new directory's parent before a file is
//! renamed into the directory, and syncs the chunk's directory after its
//! rename. A staged write leaves the directories to the commit's `syncfs`.
//! These tests read the order of the steps a write takes, from every thread.
//! A power cut is not simulated.

use std::path::{Path, PathBuf};

use wyrd_cas::{Cas, ChunkHash, Step, record_steps};

fn parent(p: &Path) -> PathBuf {
    p.parent().unwrap().to_path_buf()
}

/// The first step after `from` that makes `dir`'s entries durable: a sync of
/// `dir` itself, or a `syncfs`, which covers everything.
fn durable_after(steps: &[Step], from: usize, dir: &Path) -> Option<usize> {
    steps[from..]
        .iter()
        .position(|t| *t == Step::SyncDir(dir.to_path_buf()) || *t == Step::SyncFs)
        .map(|k| from + k)
}

/// The invariant every durable write must keep, checked on its steps:
/// - each directory made is durable in its parent (a sync of the parent, or a
///   `syncfs`) before any file is renamed into it;
/// - each rename into a directory is followed by a durable sync of it.
fn check_invariant(steps: &[Step]) {
    for (i, s) in steps.iter().enumerate() {
        match s {
            Step::Mkdir(d) => {
                let parent_dir = parent(d);
                let synced = durable_after(steps, i + 1, &parent_dir).unwrap_or_else(|| {
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
                    "a file was renamed into {} before its parent was synced: {steps:?}",
                    d.display()
                );
            }
            Step::Rename(_, to) => {
                let dir = parent(to);
                assert!(
                    durable_after(steps, i + 1, &dir).is_some(),
                    "rename into {} is never followed by a sync of it: {steps:?}",
                    dir.display()
                );
            }
            _ => {}
        }
    }
}

#[test]
fn a_new_fan_out_directory_is_synced_into_its_parent_before_the_chunk_is_renamed_in() {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let content = b"a chunk whose fan-out directories do not exist yet".to_vec();
    let h = ChunkHash::of(&content);
    let hex = h.to_hex();
    let chunks = cas.root().join("chunks");
    let ab = chunks.join(&hex[..2]);
    let cd = ab.join(&hex[2..4]);
    let chunk = cd.join(&hex);

    let (res, steps) = record_steps(|| cas.put(&content));
    assert_eq!(res.unwrap(), h);
    let tmp_file = match &steps[4] {
        Step::SyncFile(t) => t.clone(),
        other => panic!("expected the file sync at step 4, got {other:?}: {steps:?}"),
    };
    assert_eq!(
        steps,
        vec![
            Step::Mkdir(ab.clone()),
            Step::SyncDir(chunks.clone()),
            Step::Mkdir(cd.clone()),
            Step::SyncDir(ab.clone()),
            Step::SyncFile(tmp_file.clone()),
            Step::Rename(tmp_file, chunk),
            Step::SyncDir(cd),
        ],
    );
    check_invariant(&steps);
}

#[test]
fn a_chunk_into_an_existing_directory_is_synced_after_its_rename_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let first = b"first content".to_vec();
    let h = ChunkHash::of(&first);
    cas.put(&first).unwrap();
    let hex = h.to_hex();
    let leaf = cas.root().join("chunks").join(&hex[..2]).join(&hex[2..4]);
    // Remove the chunk, so the second write runs the full path into a directory that exists.
    std::fs::remove_file(leaf.join(&hex)).unwrap();
    let (res, steps) = record_steps(|| cas.put(&first));
    assert_eq!(res.unwrap(), h);
    assert_eq!(steps.len(), 3, "no directory is made: {steps:?}");
    assert!(matches!(steps[0], Step::SyncFile(_)));
    assert!(matches!(steps[1], Step::Rename(_, _)));
    assert_eq!(steps[2], Step::SyncDir(leaf));
    check_invariant(&steps);
}

#[test]
fn every_durable_entry_point_keeps_the_order() {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let body = vec![7u8; 90_000];

    let (r, steps) = record_steps(|| cas.put_stream(&body[..]));
    r.unwrap();
    check_invariant(&steps);

    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    // Different bytes from `body`, so the snapshot writes new chunks.
    std::fs::write(
        ws.join("f"),
        (0..90_000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let (r, steps) = record_steps(|| cas.snapshot(&ws, None));
    let (m, _) = r.unwrap();
    check_invariant(&steps);
    assert!(!steps.is_empty(), "the snapshot wrote its chunks");

    let (r, steps) = record_steps(|| cas.put_manifest(&m));
    r.unwrap();
    check_invariant(&steps);
}

#[test]
fn opening_a_new_store_syncs_each_directory_and_the_format_marker_it_creates() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("deep").join("cas");
    let (res, steps) = record_steps(|| Cas::open(&root));
    res.unwrap();
    check_invariant(&steps);
    assert!(
        steps.contains(&Step::SyncDir(tmp.path().join("deep"))),
        "{steps:?}"
    );
    assert!(steps.contains(&Step::SyncDir(root.clone())), "{steps:?}");
    // The marker is written to a staging file, synced, then renamed into place.
    let marker = root.join("FORMAT");
    let sync_at = steps
        .iter()
        .position(|t| matches!(t, Step::SyncFile(_)))
        .expect("the marker's data is synced");
    let rename_at = steps
        .iter()
        .position(|t| matches!(t, Step::Rename(_, to) if *to == marker))
        .expect("the marker is renamed into place");
    assert!(sync_at < rename_at, "{steps:?}");
    assert!(
        steps[rename_at..].contains(&Step::SyncDir(root.clone())),
        "the marker's directory is synced after: {steps:?}"
    );
    // Opening an existing store creates nothing and syncs nothing.
    let (res, steps) = record_steps(|| Cas::open(&root));
    res.unwrap();
    assert!(steps.is_empty(), "{steps:?}");
}

#[test]
fn writes_made_by_snapshot_worker_threads_are_recorded_and_keep_the_order() {
    // Many distinct files: the snapshot chunks them on several worker threads,
    // each creating fan-out directories and renaming chunks in. The recording
    // is global, so the worker threads' steps are part of it.
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    for i in 0..64u32 {
        let body: Vec<u8> = (0..20_000u32)
            .map(|j| ((j ^ i.wrapping_mul(7919)) % 251) as u8)
            .collect();
        std::fs::write(ws.join(format!("f{i:02}")), body).unwrap();
    }
    let (r, steps) = record_steps(|| cas.snapshot(&ws, None));
    let (m, stats) = r.unwrap();
    assert_eq!(stats.files_read, 64);
    let renames = steps
        .iter()
        .filter(|t| matches!(t, Step::Rename(_, _)))
        .count();
    assert!(
        renames >= 64,
        "every file's chunk was renamed in, from any thread: {renames}"
    );
    check_invariant(&steps);
    assert!(cas.get_manifest(cas.put_manifest(&m).unwrap()).is_ok());
}

#[test]
fn concurrent_writers_into_one_store_keep_the_order() {
    // Several threads write distinct chunks into the same store at once, so
    // they race to create the same fan-out directories.
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let ((), steps) = record_steps(|| {
        std::thread::scope(|scope| {
            for t in 0..8u32 {
                let cas = &cas;
                scope.spawn(move || {
                    for i in 0..40u32 {
                        let body =
                            format!("thread {t} chunk {i} {}", "x".repeat(3000)).into_bytes();
                        cas.put(&body).unwrap();
                    }
                });
            }
        });
    });
    let renames = steps
        .iter()
        .filter(|t| matches!(t, Step::Rename(_, _)))
        .count();
    assert_eq!(renames, 8 * 40, "every chunk was renamed in exactly once");
    check_invariant(&steps);
}
