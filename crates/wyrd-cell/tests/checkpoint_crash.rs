#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A crash can leave a chunk at its final name with no ledger event naming
//! it. The next checkpoint must not trust that name: it writes the chunk again,
//! and the checkpoint then restores byte for byte.

use std::collections::BTreeMap;
use std::fs;

use wyrd_cas::{Cas, ChunkHash};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

fn session(tmp: &tempfile::TempDir) -> (DataDir, Session) {
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
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

/// Put a wrong chunk at a file's final chunk name, as a crash could leave it.
fn plant(data: &DataDir, h: ChunkHash, bytes: &[u8]) {
    let cas = Cas::open(data.cas()).unwrap();
    let hex = h.to_hex();
    let path = cas
        .root()
        .join("chunks")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Restore the checkpoint's tree into a fresh directory and return its files.
fn restored(
    data: &DataDir,
    manifest_hash: &str,
    dir: &std::path::Path,
) -> BTreeMap<String, Vec<u8>> {
    let cas = Cas::open(data.cas()).unwrap();
    let h = ChunkHash::from_hex(manifest_hash).unwrap();
    let m = cas.get_manifest(h).unwrap();
    cas.materialize(&m, dir).unwrap();
    let mut out = BTreeMap::new();
    for e in m
        .entries
        .iter()
        .filter(|e| e.kind == wyrd_cas::EntryKind::File)
    {
        let name = String::from_utf8(e.path.to_vec()).unwrap();
        out.insert(name.clone(), fs::read(dir.join(name)).unwrap());
    }
    out
}

fn checkpoint_with_planted_name(corrupt: &[u8]) {
    let tmp = tempfile::tempdir().unwrap();
    let (data, mut s) = session(&tmp);
    // Under 16 KiB: one chunk, named by the hash of the whole file.
    let content = b"small file the crash left half-written\n".repeat(30);
    let h = ChunkHash::of(&content);
    plant(&data, h, corrupt);
    fs::write(s.dir().upper("a").join("note.txt"), &content).unwrap();

    let cp = s
        .checkpoint(None)
        .expect("the checkpoint writes the chunk again");
    let out = tmp.path().join("restored");
    let files = restored(&data, &cp.manifest, &out);
    assert_eq!(
        files.get("note.txt").map(Vec::as_slice),
        Some(content.as_slice())
    );
    // The chunk at its name is now the intact content.
    assert_eq!(Cas::open(data.cas()).unwrap().get(h).unwrap(), content);
}

#[test]
fn a_zero_length_chunk_at_its_name_is_written_again_and_the_checkpoint_restores() {
    checkpoint_with_planted_name(b"");
}

#[test]
fn a_truncated_chunk_at_its_name_is_written_again_and_the_checkpoint_restores() {
    // The stored form of the same chunk, cut in half: a short, valid-looking prefix.
    let content = b"small file the crash left half-written\n".repeat(30);
    let scratch = tempfile::tempdir().unwrap();
    let cas = Cas::open(scratch.path().join("cas")).unwrap();
    let h = cas.put(&content).unwrap();
    let hex = h.to_hex();
    let stored = fs::read(
        cas.root()
            .join("chunks")
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(&hex),
    )
    .unwrap();
    checkpoint_with_planted_name(&stored[..stored.len() / 2]);
}
