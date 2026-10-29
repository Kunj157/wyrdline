#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

//! Every durable write syncs a new directory's parent right after making the
//! directory, and syncs the chunk's directory after its rename. These tests
//! read the order of the steps a write takes. A power cut is not simulated.

use std::path::{Path, PathBuf};

use wyrd_cas::{Cas, ChunkHash, Step, record_steps};

fn parent(p: &Path) -> PathBuf {
    p.parent().unwrap().to_path_buf()
}

/// The invariant every durable write must keep, checked on its steps:
/// - each directory made is synced in its parent after it is made, and
///   before any file is renamed into it;
/// - each rename into a directory is followed by a sync of that directory.
fn check_invariant(steps: &[Step]) {
    for (i, s) in steps.iter().enumerate() {
        match s {
            Step::Mkdir(d) => {
                let synced = steps[i + 1..]
                    .iter()
                    .position(|t| *t == Step::SyncDir(parent(d)));
                let synced = synced.unwrap_or_else(|| {
                    panic!(
                        "{} was made, its parent never synced: {steps:?}",
                        d.display()
                    )
                });
                // The parent sync comes before any rename into `d`.
                let renamed_before = steps[i + 1..i + 1 + synced]
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
                    steps[i + 1..].contains(&Step::SyncDir(dir.clone())),
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
    // Two contents whose hashes share the first two bytes: same fan-out directory.
    let first = b"first content".to_vec();
    let h = ChunkHash::of(&first);
    cas.put(&first).unwrap();
    let hex = h.to_hex();
    let leaf = cas.root().join("chunks").join(&hex[..2]).join(&hex[2..4]);
    // Remove the chunk so the second write runs the full path into a directory that exists.
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
fn opening_a_new_store_syncs_each_directory_it_creates_into_its_parent() {
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
    // Opening an existing store creates nothing and syncs nothing.
    let (res, steps) = record_steps(|| Cas::open(&root));
    res.unwrap();
    assert!(steps.is_empty(), "{steps:?}");
}
