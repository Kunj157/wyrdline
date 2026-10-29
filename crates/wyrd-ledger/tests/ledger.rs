#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use proptest::prelude::*;
use rusqlite::{Connection, params};
use wyrd_ledger::*;
use wyrd_schema::*;

fn ev(i: u64, body: EventBody) -> Event {
    Event {
        seq: 0,
        ts_mono_ns: 1_000 + i,
        ts_wall_ms: 1_760_000_000_000 + i as i64,
        process: Some(ProcessRef {
            pid: 100,
            tid: 100,
            ppid: 1,
            start_ns: 5,
            cgroup_id: 9,
        }),
        drops_since_last: 0,
        body,
    }
}

fn write_ev(i: u64) -> Event {
    ev(
        i,
        EventBody::FileWrite(FileWrite {
            file: FilePath {
                path: format!("src/f{i}.rs"),
                in_cell: true,
                truncated: false,
            },
            bytes: i * 10,
        }),
    )
}

fn sample(n: u64) -> Vec<Event> {
    (0..n).map(write_ev).collect()
}

fn key() -> HostKey {
    HostKey::from_seed([7u8; 32])
}

/// Simulates an attacker with write access: drops the guard triggers.
fn raw_attacker(path: &Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.execute_batch(
        "DROP TRIGGER IF EXISTS events_no_update; DROP TRIGGER IF EXISTS events_no_delete;
         DROP TRIGGER IF EXISTS events_no_insert_after_seal; DROP TRIGGER IF EXISTS seal_no_update;
         DROP TRIGGER IF EXISTS seal_no_delete;",
    )
    .unwrap();
    c
}

#[test]
fn append_assigns_dense_sequence_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess-1").unwrap();
    assert_eq!(l.append(sample(3)).unwrap(), 3);
    assert_eq!(l.append(sample(2)).unwrap(), 5);
    let evs = l.events_after(0, 100).unwrap();
    assert_eq!(
        evs.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    let v = verify(&path).unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(v.events, 5);
    assert!(!v.sealed);
    drop(l);
    let reopened = Ledger::open(&path).unwrap();
    assert_eq!(reopened.head().0, 5);
    assert_eq!(reopened.session_id(), "sess-1");
}

#[test]
fn two_handles_appending_keep_one_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two.wyrdledger");
    let mut a = Ledger::create(&path, "two").unwrap();
    let mut b = Ledger::open(&path).unwrap();
    a.append(sample(2)).unwrap();
    assert_eq!(b.append(sample(3)).unwrap(), 5, "b sees a's events");
    assert_eq!(a.append(sample(1)).unwrap(), 6, "a sees b's events");
    a.seal(&key(), 1).unwrap();
    let v = verify(&path).unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(v.events, 6);
}

#[test]
fn empty_ledger_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e.wyrdledger");
    Ledger::create(&path, "empty").unwrap();
    let v = verify(&path).unwrap();
    assert!(v.ok());
    assert_eq!(v.events, 0);
}

#[test]
fn create_refuses_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x");
    std::fs::write(&path, b"").unwrap();
    assert!(Ledger::create(&path, "s").is_err());
}

#[test]
fn triggers_make_events_append_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wyrdledger");
    let mut l = Ledger::create(&path, "s").unwrap();
    l.append(sample(2)).unwrap();
    let c = Connection::open(&path).unwrap();
    assert!(
        c.execute("UPDATE events SET kind = 'x' WHERE seq = 1", [])
            .is_err()
    );
    assert!(c.execute("DELETE FROM events WHERE seq = 2", []).is_err());
}

#[test]
fn sealed_ledger_rejects_appends_and_verifies_signature() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess").unwrap();
    l.append(sample(4)).unwrap();
    let seal = l.seal(&key(), 1).unwrap();
    assert_eq!(seal.head_seq, 4);
    assert!(matches!(l.append(sample(1)), Err(LedgerError::Sealed)));
    // Even raw SQL cannot append once sealed.
    let c = Connection::open(&path).unwrap();
    assert!(
        c.execute(
            "INSERT INTO events (seq, kind, plane, ts_mono_ns, event_cbor, prev_hash, hash)
             VALUES (5, 'k', 'p', 0, x'00', x'00', x'00')",
            []
        )
        .is_err()
    );
    let v = verify(&path).unwrap();
    assert!(v.ok(), "{v:?}");
    assert!(v.sealed);
    assert_eq!(
        v.signer_fingerprint.as_deref(),
        Some(key().fingerprint().as_str())
    );
}

#[test]
fn forged_signature_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess").unwrap();
    l.append(sample(2)).unwrap();
    l.seal(&key(), 1).unwrap();
    drop(l);
    let c = raw_attacker(&path);
    let other = HostKey::from_seed([9u8; 32]);
    c.execute(
        "UPDATE seal SET public_key = ?1",
        [other.public_key().as_slice()],
    )
    .unwrap();
    assert_eq!(verify(&path).unwrap().problem, Some(Problem::BadSignature));
}

#[test]
fn appending_after_seal_by_attacker_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess").unwrap();
    l.append(sample(2)).unwrap();
    l.seal(&key(), 1).unwrap();
    drop(l);
    // Attacker removes the seal, appends a correctly chained event, restores the seal.
    let c = raw_attacker(&path);
    let seal: (i64, Vec<u8>, Vec<u8>, Vec<u8>, i64) = c
        .query_row(
            "SELECT head_seq, head_hash, public_key, signature, sealed_at_ms FROM seal",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    c.execute("DELETE FROM seal", []).unwrap();
    drop(c);
    let mut l = Ledger::open(&path).unwrap();
    l.append(sample(1)).unwrap();
    drop(l);
    let c = raw_attacker(&path);
    c.execute(
        "INSERT INTO seal VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![seal.0, seal.1, seal.2, seal.3, seal.4],
    )
    .unwrap();
    assert_eq!(
        verify(&path).unwrap().problem,
        Some(Problem::SealHeadMismatch {
            sealed_seq: 2,
            actual_seq: 3
        })
    );
}

#[test]
fn deleted_row_is_a_sequence_gap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    Ledger::create(&path, "s")
        .unwrap()
        .append(sample(5))
        .unwrap();
    raw_attacker(&path)
        .execute("DELETE FROM events WHERE seq = 3", [])
        .unwrap();
    assert_eq!(
        verify(&path).unwrap().problem,
        Some(Problem::SequenceGap {
            expected: 3,
            found: 4
        })
    );
}

#[test]
fn mismatched_index_column_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    Ledger::create(&path, "s")
        .unwrap()
        .append(sample(3))
        .unwrap();
    raw_attacker(&path)
        .execute(
            "UPDATE events SET kind = 'effect.net.connect' WHERE seq = 2",
            [],
        )
        .unwrap();
    assert!(matches!(
        verify(&path).unwrap().problem,
        Some(Problem::BadEvent { seq: 2, .. })
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Flipping any byte of any stored event, prev_hash, or hash is detected
    /// at exactly that sequence number.
    #[test]
    fn any_single_byte_tamper_is_detected_at_its_seq(
        n in 1u64..12,
        pick in any::<prop::sample::Index>(),
        column in 0usize..3,
        byte in any::<prop::sample::Index>(),
        flip in 1u8..=255,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.wyrdledger");
        Ledger::create(&path, "p").unwrap().append(sample(n)).unwrap();
        let seq = pick.index(n as usize) as i64 + 1;
        let col = ["event_cbor", "prev_hash", "hash"][column];
        let c = raw_attacker(&path);
        let mut blob: Vec<u8> = c
            .query_row(&format!("SELECT {col} FROM events WHERE seq = ?1"), [seq], |r| r.get(0))
            .unwrap();
        let i = byte.index(blob.len());
        blob[i] ^= flip;
        c.execute(&format!("UPDATE events SET {col} = ?1 WHERE seq = ?2"), params![blob, seq]).unwrap();
        drop(c);
        let v = verify(&path).unwrap();
        let at = match v.problem {
            Some(Problem::BrokenLink { seq } | Problem::HashMismatch { seq } | Problem::BadEvent { seq, .. }) => seq,
            other => panic!("unexpected {other:?}"),
        };
        // A changed hash is noticed either at its own row (mismatch) or, for the
        // last row only, there; a changed prev_hash/event at its own row.
        prop_assert_eq!(at, seq as u64);
    }
}

#[test]
fn projections_survive_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.wyrdledger");
    let mut l = Ledger::create(&path, "r").unwrap();
    l.append(vec![
        ev(
            0,
            EventBody::ToolCallStarted(ToolCallStarted {
                call_id: "c1".into(),
                source: IntentSource::Hook,
                tool: "Bash".into(),
                args: serde_json::json!({"command": "pnpm test"}),
                turn: Some(3),
            }),
        ),
        write_ev(1),
        ev(
            2,
            EventBody::ToolCallFinished(ToolCallFinished {
                call_id: "c1".into(),
                status: CallStatus::Error,
                exit_code: Some(1),
                summary: None,
                duration_ms: 42,
            }),
        ),
        ev(
            3,
            EventBody::CheckpointCreated(CheckpointCreated {
                checkpoint_id: "k1".into(),
                branch_id: "a".into(),
                parent_id: None,
                after_call_id: Some("c1".into()),
                manifest: "b3:00".into(),
                files_changed: 1,
                bytes_added: 10,
                duration_ms: 3,
            }),
        ),
        ev(
            4,
            EventBody::DivergenceRaised(DivergenceRaised {
                divergence_id: "d1".into(),
                rule_id: "D005".into(),
                severity: Severity::High,
                claim_id: None,
                evidence_seqs: vec![1, 3],
                confidence_bp: 9_000,
                summary: "silent failure".into(),
            }),
        ),
        ev(
            5,
            EventBody::DivergenceUpdated(DivergenceUpdated {
                divergence_id: "d1".into(),
                status: DivergenceStatus::Acknowledged,
                note: Some("known".into()),
            }),
        ),
    ])
    .unwrap();
    let snapshot = |l: &Ledger| -> Vec<String> {
        let mut out = Vec::new();
        for q in [
            "SELECT call_id || tool || status || exit_code || ended_seq FROM tool_calls",
            "SELECT checkpoint_id || seq || manifest FROM checkpoints",
            "SELECT divergence_id || status || evidence_json || note FROM divergences",
        ] {
            let mut st = l.conn().prepare(q).unwrap();
            out.extend(
                st.query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .map(|r| r.unwrap()),
            );
        }
        out
    };
    let before = snapshot(&l);
    assert_eq!(
        before,
        vec!["c1Basherror13", "k14b3:00", "d1acknowledged[1,3]known"]
    );
    l.reindex().unwrap();
    assert_eq!(snapshot(&l), before);
}

#[test]
fn host_key_is_created_private_and_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys/host.key");
    let a = HostKey::load_or_create(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let b = HostKey::load_or_create(&path).unwrap();
    assert_eq!(a.public_key(), b.public_key());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        HostKey::load(&path).is_err(),
        "world-readable key must be refused"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_batches_and_flushes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.wyrdledger");
    let w = spawn_writer(Ledger::create(&path, "w").unwrap(), 4096);
    for i in 0..10_000 {
        w.submit(write_ev(i)).await.unwrap();
    }
    assert_eq!(w.flush().await.unwrap(), 10_000);
    let mut l = w.close().await.unwrap();
    l.seal(&key(), 2).unwrap();
    assert!(verify(&path).unwrap().ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_reports_failed_commits_on_flush() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.wyrdledger");
    let mut l = Ledger::create(&path, "f").unwrap();
    l.seal(&key(), 1).unwrap();
    let w = spawn_writer(l, 16);
    w.submit(write_ev(0)).await.unwrap();
    assert!(matches!(
        w.flush().await,
        Err(LedgerError::Sealed | LedgerError::Commit(_))
    ));
}

/// Child half of the crash test: appends forever until killed.
#[test]
#[ignore = "spawned by kill_during_append_leaves_valid_chain"]
fn crash_child() {
    let Ok(path) = std::env::var("WYRD_CRASH_LEDGER") else {
        return;
    };
    let mut l = Ledger::open(&path).unwrap();
    let mut i = 0;
    loop {
        l.append((i..i + 500).map(write_ev).collect()).unwrap();
        i += 500;
    }
}

#[test]
fn kill_during_append_leaves_valid_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.wyrdledger");
    Ledger::create(&path, "crash").unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--ignored", "--nocapture"])
        .env("WYRD_CRASH_LEDGER", &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    child.kill().unwrap(); // SIGKILL
    child.wait().unwrap();
    let v = verify(&path).unwrap();
    assert!(v.ok(), "{v:?}");
    assert!(v.events > 0, "child should have committed something");
    assert_eq!(v.events % 500, 0, "partial batches must never be visible");
}

#[test]
#[ignore = "throughput benchmark; run with `make test-perf` (release)"]
fn throughput_at_least_150k_events_per_second() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("perf.wyrdledger");
    let mut l = Ledger::create(&path, "perf").unwrap();
    let total = 300_000u64;
    let start = std::time::Instant::now();
    for chunk in 0..(total / 1_000) {
        l.append((chunk * 1_000..(chunk + 1) * 1_000).map(write_ev).collect())
            .unwrap();
    }
    let rate = total as f64 / start.elapsed().as_secs_f64();
    eprintln!("ledger ingest: {rate:.0} events/s");
    assert!(rate >= 150_000.0, "{rate:.0} events/s");
}

#[test]
fn sync_moves_the_appended_events_into_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.wyrdledger");
    let mut l = Ledger::create(&path, "sess-sync").unwrap();
    l.append(sample(3)).unwrap();
    let wal = dir.path().join("s.wyrdledger-wal");
    // Appends are in the write-ahead log, not yet in the database file.
    assert!(
        std::fs::metadata(&wal).map_or(0, |m| m.len()) > 0,
        "the appends are in the log"
    );
    l.sync().unwrap();
    // The checkpoint resets the log, so the events now live in the database file.
    assert_eq!(
        std::fs::metadata(&wal).map_or(0, |m| m.len()),
        0,
        "the log was reset"
    );
    assert_eq!(l.events_after(0, 100).unwrap().len(), 3);
    verify(&path).unwrap();
}
