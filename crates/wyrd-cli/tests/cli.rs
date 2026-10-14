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
