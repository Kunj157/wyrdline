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

/// A `sudo` on a temporary PATH whose exit status and stderr the test chooses.
fn fake_sudo_on_path(exit: i32, stderr: &str) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let sudo = dir.path().join("sudo");
    std::fs::write(
        &sudo,
        format!("#!/bin/sh\necho '{stderr}' >&2\nexit {exit}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (dir, path)
}

#[test]
fn doctor_probes_the_privileged_helper_with_sudo_l() {
    let (_sudo, path) = fake_sudo_on_path(0, "");
    let out = wyrd()
        .args(["doctor", "--json"])
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("PATH", path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["privileged_helper"], "verified");
}

#[test]
fn doctor_reports_a_refused_privileged_helper() {
    let (_sudo, path) = fake_sudo_on_path(1, "sudo: a password is required");
    let out = wyrd()
        .args(["doctor", "--json"])
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("PATH", path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["privileged_helper"]["refused"],
        "sudo: a password is required"
    );
    // The helper must not be the path that enables Cell mode.
    assert_ne!(v["cell_path"], "privileged_helper");
}

#[test]
fn doctor_does_not_run_sudo_unless_the_helper_is_enabled() {
    // A sudo that records that it ran. It must not run without the variable.
    let (sudo_dir, path) = fake_sudo_on_path(1, "");
    let marker = sudo_dir.path().join("ran");
    std::fs::write(
        sudo_dir.path().join("sudo"),
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    let out = wyrd()
        .args(["doctor", "--json"])
        .env_remove("WYRD_PRIVILEGED_HELPER")
        .env("PATH", path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["privileged_helper"], "not_configured");
    assert!(!marker.exists(), "sudo ran without WYRD_PRIVILEGED_HELPER");
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

#[test]
fn doctor_probes_the_sensor_socket_named_by_the_environment() {
    // The sensor client honours WYRD_SENSORD_SOCKET; the doctor must check
    // the same socket, not the system default.
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("sensord.sock");
    std::fs::write(&sock, "").unwrap();
    let out = wyrd()
        .args(["doctor", "--json"])
        .env("WYRD_SENSORD_SOCKET", &sock)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let check = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "sensord")
        .expect("the sensord check");
    assert_eq!(check["status"], "ok", "{check}");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .contains(&sock.display().to_string())
    );
}

#[test]
fn doctor_and_sensor_client_resolve_the_same_socket() {
    // The doctor keeps its own copy of the client's rule, because wyrdd must not
    // depend on the sensor crate (its eBPF build script). This keeps them equal.
    assert_eq!(
        wyrd_doctor::sensord_socket_path(),
        wyrd_sensor::client::socket_path()
    );
}

#[test]
fn gc_deletes_unreferenced_chunks_and_a_dry_run_only_reports_them() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    wyrd_cas::Cas::open(data.join("cas"))
        .unwrap()
        .put(b"nobody refers to this")
        .unwrap();
    let run = |args: &[&str]| {
        let out = wyrd()
            .arg("gc")
            .args(args)
            .env("WYRD_DATA_DIR", &data)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };

    let v = run(&["--dry-run", "--json"]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["collectable"], 1);
    assert!(data.join("cas/chunks").exists());

    let v = run(&["--json"]);
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["deleted"], 1);
    assert!(v["bytes_freed"].as_u64().unwrap() > 0);

    let v = run(&["--json"]);
    assert_eq!(v["deleted"], 0, "a second collection has nothing to free");
}
