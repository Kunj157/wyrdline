#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn wyrd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wyrd"))
}

#[test]
fn version_prints_binary_name_and_version() {
    let out = wyrd().arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), format!("wyrd {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn doctor_json_lists_every_check() {
    let out = wyrd().args(["doctor", "--json"]).output().unwrap();
    let code = out.status.code().unwrap();
    assert!(code == 0 || code == 2, "unexpected exit code {code}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["checks"].as_array().unwrap().len(), 11);
    assert_eq!(v["cell_mode"].as_bool().unwrap(), code == 0);
}

#[test]
fn doctor_mask_makes_cell_mode_unavailable() {
    let out = wyrd()
        .args(["doctor", "--json"])
        .env("WYRD_DOCTOR_MASK", "landlock")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["masked"][0], "landlock");
    assert_eq!(v["cell_mode"], false);
}

#[test]
fn doctor_text_output_has_summary_line() {
    let out = wyrd().arg("doctor").output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Cell mode:"));
    assert!(stdout.contains("Landlock LSM"));
}

fn sample_ledger(dir: &std::path::Path) -> std::path::PathBuf {
    use wyrd_schema::{Event, EventBody, FilePath, FileWrite};
    let path = dir.join("s.wyrdledger");
    let mut l = wyrd_ledger::Ledger::create(&path, "cli-test").unwrap();
    let events = (0..3)
        .map(|i| Event {
            seq: 0,
            ts_mono_ns: i,
            ts_wall_ms: 0,
            process: None,
            drops_since_last: 0,
            body: EventBody::FileWrite(FileWrite {
                file: FilePath {
                    path: format!("f{i}"),
                    in_cell: true,
                    truncated: false,
                },
                bytes: 1,
            }),
        })
        .collect();
    l.append(events).unwrap();
    l.seal(&wyrd_ledger::HostKey::from_seed([1; 32]), 0)
        .unwrap();
    path
}

#[test]
fn verify_reports_intact_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = sample_ledger(dir.path());
    let out = wyrd().arg("verify").arg(&path).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("chain intact"), "{stdout}");
    assert!(stdout.contains("events    3"));
}

#[test]
fn verify_pinpoints_tampering_with_exit_code_3() {
    let dir = tempfile::tempdir().unwrap();
    let path = sample_ledger(dir.path());
    let c = rusqlite_free_tamper(&path);
    assert!(c);
    let out = wyrd()
        .args(["verify", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["problem"]["problem"], "hash_mismatch");
    assert_eq!(v["problem"]["seq"], 2);
}

/// Flip a byte inside the stored hash of seq 2 by patching the file through
/// the ledger crate's own connection after dropping the guard trigger.
fn rusqlite_free_tamper(path: &std::path::Path) -> bool {
    let l = wyrd_ledger::Ledger::open(path).unwrap();
    l.conn()
        .execute_batch("DROP TRIGGER events_no_update;")
        .unwrap();
    l.conn()
        .execute("UPDATE events SET hash = zeroblob(32) WHERE seq = 2", [])
        .unwrap()
        == 1
}

#[test]
fn verify_rejects_non_ledger_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-a-ledger");
    std::fs::write(&path, b"hello").unwrap();
    let out = wyrd().arg("verify").arg(&path).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}
