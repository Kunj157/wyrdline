#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use proptest::prelude::*;
use serde_bytes::ByteBuf;
use wyrd_cas::*;

/// Hash of every path in a tree: content for files, target for symlinks, mode for all.
fn fingerprint(dir: &Path) -> BTreeMap<Vec<u8>, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().as_os_str().as_bytes().to_vec();
            let m = fs::symlink_metadata(&p).unwrap();
            let mode = m.permissions().mode() & 0o7777;
            let v = if m.is_symlink() {
                format!("link:{}", fs::read_link(&p).unwrap().display())
            } else if m.is_dir() {
                stack.push(p.clone());
                format!("dir:{mode:o}")
            } else {
                format!(
                    "file:{mode:o}:{}",
                    blake3::hash(&fs::read(&p).unwrap()).to_hex()
                )
            };
            out.insert(rel, v);
        }
    }
    out
}

#[derive(Debug, Clone)]
enum Node {
    File(Vec<u8>, u32),
    Link(String),
    Dir(Vec<(Vec<u8>, Node)>),
}

fn name() -> impl Strategy<Value = Vec<u8>> {
    // Includes non-UTF-8 bytes; never '/', NUL, '.' or '..'.
    prop::collection::vec(
        prop_oneof![b'a'..=b'z', Just(0xffu8), Just(b'-'), Just(b' ')],
        1..8,
    )
    .prop_filter("no dot names", |n| n != b"." && n != b"..")
}

fn node() -> impl Strategy<Value = Node> {
    let leaf = prop_oneof![
        (
            prop::collection::vec(any::<u8>(), 0..70_000),
            prop_oneof![Just(0o644u32), Just(0o755), Just(0o600)]
        )
            .prop_map(|(d, m)| Node::File(d, m)),
        "[a-z/.]{1,12}".prop_map(Node::Link),
    ];
    leaf.prop_recursive(3, 24, 5, |inner| {
        prop::collection::btree_map(name(), inner, 0..5)
            .prop_map(|m| Node::Dir(m.into_iter().collect()))
    })
}

fn build(dir: &Path, children: &[(Vec<u8>, Node)]) {
    for (n, node) in children {
        let p = dir.join(std::ffi::OsStr::from_bytes(n));
        match node {
            Node::File(d, m) => {
                fs::write(&p, d).unwrap();
                fs::set_permissions(&p, fs::Permissions::from_mode(*m)).unwrap();
            }
            Node::Link(t) => std::os::unix::fs::symlink(t, &p).unwrap(),
            Node::Dir(c) => {
                fs::create_dir(&p).unwrap();
                build(&p, c);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn snapshot_materialize_round_trip(tree in prop::collection::btree_map(name(), node(), 0..6)) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        build(&src, &tree.into_iter().collect::<Vec<_>>());
        let cas = Cas::open(tmp.path().join("cas")).unwrap();
        let (m, _) = cas.snapshot(&src, None).unwrap();
        let h = cas.put_manifest(&m).unwrap();
        let back = cas.get_manifest(h).unwrap();
        prop_assert_eq!(&back, &m);
        let dst = tmp.path().join("dst");
        cas.materialize(&back, &dst).unwrap();
        prop_assert_eq!(fingerprint(&src), fingerprint(&dst));
        // A second snapshot of the copy yields the same manifest (mtimes restored).
        let (again, _) = cas.snapshot(&dst, None).unwrap();
        prop_assert_eq!(again, m);
    }
}

#[test]
fn manifest_hash_is_stable_and_dedups_content() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    let big: Vec<u8> = (0..600_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    fs::write(src.join("a.bin"), &big).unwrap();
    fs::write(src.join("b.bin"), &big).unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let (m1, s1) = cas.snapshot(&src, None).unwrap();
    assert_eq!(s1.files_read, 2);
    let a = m1.get(b"a.bin").unwrap();
    let b = m1.get(b"b.bin").unwrap();
    assert_eq!(a.chunks, b.chunks, "identical files share chunks");
    assert!(
        a.chunks.len() > 2,
        "600 KB file is split into several chunks"
    );
    let h1 = cas.put_manifest(&m1).unwrap();
    // Incremental snapshot with base: nothing re-read, same manifest hash.
    let (m2, s2) = cas.snapshot(&src, Some(&m1)).unwrap();
    assert_eq!(s2.files_read, 0);
    assert_eq!(cas.put_manifest(&m2).unwrap(), h1);
    // Change one byte in the middle: only nearby chunks change.
    let mut changed = big.clone();
    changed[300_000] ^= 1;
    fs::write(src.join("a.bin"), &changed).unwrap();
    let (m3, s3) = cas.snapshot(&src, Some(&m2)).unwrap();
    assert_eq!(s3.files_read, 1);
    let new = &m3.get(b"a.bin").unwrap().chunks;
    let shared = new.iter().filter(|c| a.chunks.contains(c)).count();
    assert!(
        shared >= new.len() - 2,
        "content-defined chunking limits the change"
    );
}

#[test]
fn large_sparse_file_is_streamed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    let mut f = fs::File::create(src.join("sparse.img")).unwrap();
    f.seek(SeekFrom::Start(256 * 1024 * 1024 - 1)).unwrap();
    f.write_all(b"x").unwrap();
    drop(f);
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let (m, stats) = cas.snapshot(&src, None).unwrap();
    assert_eq!(stats.bytes_read, 256 * 1024 * 1024);
    let e = m.get(b"sparse.img").unwrap();
    assert_eq!(e.size, 256 * 1024 * 1024);
    let unique: std::collections::HashSet<_> = e.chunks.iter().collect();
    assert!(
        unique.len() <= 3,
        "zero runs deduplicate to a handful of chunks"
    );
}

#[test]
fn corrupt_and_missing_chunks_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let h = cas.put(b"hello world").unwrap();
    assert_eq!(cas.get(h).unwrap(), b"hello world");
    let hex = h.to_hex();
    let path = tmp
        .path()
        .join("cas/chunks")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex);
    let other = zstd::bulk::compress(b"hello WORLD", 3).unwrap();
    fs::write(&path, other).unwrap();
    assert!(matches!(cas.get(h), Err(CasError::Corrupt(_))));
    fs::write(&path, b"not zstd").unwrap();
    assert!(matches!(cas.get(h), Err(CasError::Corrupt(_))));
    fs::remove_file(&path).unwrap();
    assert!(matches!(cas.get(h), Err(CasError::Missing(_))));
}

fn entry(path: &str, kind: EntryKind) -> Entry {
    Entry {
        path: ByteBuf::from(path.as_bytes().to_vec()),
        kind,
        mode: 0o755,
        size: 0,
        mtime_ns: 0,
        chunks: vec![],
        target: (kind == EntryKind::Symlink).then(|| ByteBuf::from(b"/etc".to_vec())),
        opaque: false,
    }
}

#[test]
fn unsafe_manifests_are_rejected() {
    for bad in ["../x", "/abs", "a//b", "a/./b", "", "a/../b"] {
        assert!(
            Manifest::new(vec![entry(bad, EntryKind::File)]).is_err(),
            "{bad:?}"
        );
    }
    // A file below a symlink would escape the destination on materialise.
    let r = Manifest::new(vec![
        entry("link", EntryKind::Symlink),
        entry("link/x", EntryKind::File),
    ]);
    assert!(matches!(r, Err(CasError::UnsafePath(_))));
    // A file whose parent directory is not listed.
    assert!(Manifest::new(vec![entry("d/x", EntryKind::File)]).is_err());
    // Duplicates.
    assert!(
        Manifest::new(vec![
            entry("a", EntryKind::File),
            entry("a", EntryKind::File)
        ])
        .is_err()
    );
    // Valid nested tree.
    assert!(
        Manifest::new(vec![
            entry("d", EntryKind::Dir),
            entry("d/x", EntryKind::File)
        ])
        .is_ok()
    );
    // Garbage CBOR.
    assert!(Manifest::from_cbor(b"\xff\x00garbage").is_err());
}

#[test]
fn materialize_refuses_non_empty_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let dst = tmp.path().join("dst");
    fs::create_dir(&dst).unwrap();
    fs::write(dst.join("existing"), b"x").unwrap();
    assert!(cas.materialize(&Manifest::default(), &dst).is_err());
}

#[test]
fn whiteouts_and_opaque_dirs_are_reported_for_the_cell() {
    let tmp = tempfile::tempdir().unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let mut d = entry("gone-dir", EntryKind::Dir);
    d.opaque = true;
    let m = Manifest::new(vec![d, entry("deleted.txt", EntryKind::Whiteout)]).unwrap();
    let r = cas.materialize(&m, &tmp.path().join("dst")).unwrap();
    assert_eq!(r.whiteouts, vec![tmp.path().join("dst/deleted.txt")]);
    assert_eq!(r.opaque_dirs, vec![tmp.path().join("dst/gone-dir")]);
}

#[test]
fn gc_keeps_reachable_chunks_only() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("keep.txt"), b"keep me").unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let (m, _) = cas.snapshot(&src, None).unwrap();
    let root = cas.put_manifest(&m).unwrap();
    let orphan = cas.put(b"orphan chunk").unwrap();
    let dry = cas.gc(&[root], true).unwrap();
    assert_eq!((dry.kept, dry.deleted), (2, 1));
    assert!(cas.contains(orphan), "dry run deletes nothing");
    let real = cas.gc(&[root], false).unwrap();
    assert_eq!(real.deleted, 1);
    assert!(!cas.contains(orphan));
    let dst = tmp.path().join("dst");
    cas.materialize(&cas.get_manifest(root).unwrap(), &dst)
        .unwrap();
    assert_eq!(fs::read(dst.join("keep.txt")).unwrap(), b"keep me");
}

#[test]
fn special_files_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        src.join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    let cas = Cas::open(tmp.path().join("cas")).unwrap();
    let (m, stats) = cas.snapshot(&src, None).unwrap();
    assert_eq!(stats.skipped_special, 1);
    assert_eq!(m.entries.len(), 0);
}
