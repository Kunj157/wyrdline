#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_wrap
)]

//! `Ledger::snapshot_to`: a consistent single-file copy of a ledger, taken
//! while the ledger is open and may still hold events only in its write-ahead
//! log (#318).

use wyrd_ledger::*;
use wyrd_schema::*;

fn write_ev(i: u64) -> Event {
    Event {
        seq: 0,
        ts_mono_ns: 1_000 + i,
        ts_wall_ms: 1_760_000_000_000 + i as i64,
        process: None,
        drops_since_last: 0,
        body: EventBody::FileWrite(FileWrite {
            file: FilePath {
                path: format!("src/f{i}.rs"),
                in_cell: true,
                truncated: false,
            },
            bytes: i,
        }),
    }
}

fn sample(n: u64) -> Vec<Event> {
    (0..n).map(write_ev).collect()
}

fn key() -> HostKey {
    HostKey::from_seed([7u8; 32])
}

#[test]
fn a_snapshot_of_an_open_ledger_holds_events_still_in_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess-snap").unwrap();
    l.append_durable(sample(4)).unwrap();
    // No checkpoint has run: the events are in the -wal file, not the database.
    let dest = dir.path().join("copy.wyrdledger");
    l.snapshot_to(&dest).unwrap();
    let v = verify(&dest).unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(v.events, 4);
    assert_eq!(v.head_hash, verify(&path).unwrap().head_hash);
    assert_eq!(
        Ledger::open_read_only(&dest).unwrap().session_id(),
        "sess-snap"
    );
    assert!(
        !dir.path().join("copy.wyrdledger-wal").exists(),
        "the copy is one file"
    );
}

#[test]
fn a_sealed_snapshot_still_verifies_its_signature() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess-sealed").unwrap();
    l.append(sample(3)).unwrap();
    l.seal(&key(), 42).unwrap();
    let dest = dir.path().join("copy.wyrdledger");
    Ledger::open_read_only(&path)
        .unwrap()
        .snapshot_to(&dest)
        .unwrap();
    let v = verify(&dest).unwrap();
    assert!(v.ok(), "{v:?}");
    assert!(v.sealed);
    assert_eq!(
        v.signer_fingerprint.as_deref(),
        Some(key().fingerprint().as_str())
    );
}

#[test]
fn a_snapshot_refuses_to_overwrite_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let l = Ledger::create(&path, "sess-exists").unwrap();
    let dest = dir.path().join("taken");
    std::fs::write(&dest, b"occupied").unwrap();
    assert!(l.snapshot_to(&dest).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"occupied");
}
