use std::process::Command;

#[test]
fn version_prints_binary_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), format!("wyrd {}", env!("CARGO_PKG_VERSION")));
}
