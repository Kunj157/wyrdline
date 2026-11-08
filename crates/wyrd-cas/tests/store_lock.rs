#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The store lock (#296): writers that add chunks and then record a reference
//! hold it shared, and `gc` holds it exclusive while it computes its roots and
//! sweeps. These tests pin what each holder blocks.

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use wyrd_cas::{Cas, CasError, ChunkHash, Step, record_steps};

fn store() -> (tempfile::TempDir, Cas) {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    (tmp, cas)
}

/// Store a directory holding one file with `body`; returns its manifest hash.
fn checkpoint(cas: &Cas, tmp: &tempfile::TempDir, body: &[u8]) -> ChunkHash {
    let dir = tmp
        .path()
        .join(format!("src-{}", ChunkHash::of(body).to_hex()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f"), body).unwrap();
    let (m, _) = cas.snapshot(&dir, None).unwrap();
    cas.put_manifest(&m).unwrap()
}

#[test]
fn gc_deletes_only_what_no_root_reaches() {
    let (tmp, cas) = store();
    let kept = checkpoint(&cas, &tmp, b"referenced content");
    let orphan = cas.put(b"nobody refers to this").unwrap();
    let lock = cas.lock_exclusive(Duration::ZERO).unwrap();

    let dry = cas.gc(&lock, &[kept], true).unwrap();
    assert_eq!((dry.kept, dry.deleted), (2, 1), "{dry:?}");
    assert!(cas.contains(orphan), "a dry run deletes nothing");

    let r = cas.gc(&lock, &[kept], false).unwrap();
    assert_eq!((r.kept, r.deleted), (2, 1), "{r:?}");
    assert!(r.bytes_freed > 0);
    assert!(!cas.contains(orphan));
    assert!(cas.contains(kept));
    let file_chunk = cas.get_manifest(kept).unwrap().entries[0].chunks[0];
    assert_eq!(cas.get(file_chunk).unwrap(), b"referenced content");
}

#[test]
fn gc_refuses_an_unreadable_root_and_deletes_nothing() {
    let (_tmp, cas) = store();
    let orphan = cas.put(b"orphan").unwrap();
    let lock = cas.lock_exclusive(Duration::ZERO).unwrap();
    let missing = ChunkHash::of(b"never stored");
    assert!(cas.gc(&lock, &[missing], false).is_err());
    assert!(cas.contains(orphan));
}

#[test]
fn a_shared_holder_blocks_gc_for_the_bounded_wait() {
    let (_tmp, cas) = store();
    let _writer = cas.lock_shared().unwrap();
    let got = cas.lock_exclusive(Duration::from_millis(50));
    assert!(
        matches!(got, Err(CasError::Busy(_))),
        "gc got the lock while a writer held it"
    );
}

#[test]
fn shared_holders_do_not_block_each_other() {
    let (_tmp, cas) = store();
    let _a = cas.lock_shared().unwrap();
    let _b = cas.lock_shared().unwrap();
    assert!(matches!(
        cas.lock_exclusive(Duration::ZERO),
        Err(CasError::Busy(_))
    ));
}

#[test]
fn an_exclusive_holder_blocks_a_writer_until_it_is_released() {
    let (_tmp, cas) = store();
    let gc_lock = cas.lock_exclusive(Duration::ZERO).unwrap();
    let (tx, rx) = mpsc::channel();
    let writer = cas.clone();
    std::thread::spawn(move || {
        let _held = writer.lock_shared().unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "a writer got in while gc held the store"
    );
    drop(gc_lock);
    rx.recv_timeout(Duration::from_secs(10))
        .expect("the writer never got the lock after gc finished");
}

#[test]
fn the_lock_records_its_acquire_and_release() {
    let (_tmp, cas) = store();
    let (held, steps) = record_steps(|| cas.lock_shared().unwrap());
    assert_eq!(steps, vec![Step::Lock { exclusive: false }]);
    let ((), steps) = record_steps(|| drop(held));
    assert_eq!(steps, vec![Step::Unlock]);
}

#[test]
fn gc_sweeps_staging_files_of_dead_processes_only() {
    let (_tmp, cas) = store();
    let hex = ChunkHash::of(b"staged").to_hex();
    let child = Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    let mut child = child;
    child.wait().unwrap();
    let tmp = cas.root().join("tmp");
    let dead = tmp.join(format!("{hex}.{dead_pid}.0"));
    let own = tmp.join(format!("{hex}.{}.1", std::process::id()));
    std::fs::write(&dead, b"partial").unwrap();
    std::fs::write(&own, b"in progress").unwrap();
    let lock = cas.lock_exclusive(Duration::ZERO).unwrap();

    cas.gc(&lock, &[], true).unwrap();
    assert!(dead.exists(), "a dry run removes nothing");

    cas.gc(&lock, &[], false).unwrap();
    assert!(!dead.exists(), "a dead process's staging file is removed");
    assert!(own.exists(), "a live process's staging file is kept");
}
