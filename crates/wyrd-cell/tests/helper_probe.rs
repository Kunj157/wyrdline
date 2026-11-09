//! The `sudo -n -l` probe of the privileged helper, against fake `sudo`
//! scripts: no real sudo, no privileges.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use wyrd_cell::launch::probe_sudo_helper_via;

/// Write an executable `sudo` script with `body` and return its path.
fn fake_sudo(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("sudo");
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Accepts exactly `sudo -n -l -- <helper> __cell-exec <spec>` and nothing else.
const EXPECTED_SHAPE: &str = r#"
[ "$1" = -n ] && [ "$2" = -l ] && [ "$3" = -- ] && [ "$5" = __cell-exec ] && [ "$#" = 6 ] || { echo "bad argv: $*" >&2; exit 3; }
"#;

#[test]
fn allowed_rule_is_verified_and_runs_the_listing_shape() {
    let dir = tempfile::tempdir().unwrap();
    let sudo = fake_sudo(dir.path(), &format!("{EXPECTED_SHAPE}\nexit 0"));
    assert_eq!(
        probe_sudo_helper_via(&sudo, Path::new("/usr/local/bin/wyrd")),
        Ok(())
    );
}

#[test]
fn password_required_is_refused_with_sudos_own_reason() {
    let dir = tempfile::tempdir().unwrap();
    let sudo = fake_sudo(
        dir.path(),
        &format!("{EXPECTED_SHAPE}\necho 'sudo: a password is required' >&2\nexit 1"),
    );
    assert_eq!(
        probe_sudo_helper_via(&sudo, Path::new("/usr/local/bin/wyrd")),
        Err("sudo: a password is required".to_string())
    );
}

#[test]
fn refusal_without_stderr_names_the_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let sudo = fake_sudo(dir.path(), &format!("{EXPECTED_SHAPE}\nexit 1"));
    let err = probe_sudo_helper_via(&sudo, Path::new("/usr/local/bin/wyrd")).unwrap_err();
    assert!(err.contains("exited with"), "{err}");
}

#[test]
fn missing_sudo_is_reported_not_panicked() {
    let err = probe_sudo_helper_via(
        "/nonexistent/sudo-binary-for-test",
        Path::new("/usr/local/bin/wyrd"),
    )
    .unwrap_err();
    assert!(err.starts_with("cannot run sudo"), "{err}");
}
