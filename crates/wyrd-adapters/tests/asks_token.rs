#![allow(clippy::unwrap_used, clippy::panic)]

//! The control token is replaced in one step: a reader never sees it empty
//! or half-written, even while it is being rewritten.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};

use wyrd_adapters::asks::{CONTROL_TOKEN_FILE, write_private_atomic};

#[test]
fn a_control_token_is_never_read_empty_or_partial_while_it_is_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONTROL_TOKEN_FILE);
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    write_private_atomic(&path, a.as_bytes()).unwrap();
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..300 {
                let next = if i % 2 == 0 { &b } else { &a };
                write_private_atomic(&path, next.as_bytes()).unwrap();
            }
            done.store(true, Ordering::SeqCst);
        });
        s.spawn(|| {
            // Every read, however early, is one whole token.
            while !done.load(Ordering::SeqCst) {
                let seen = std::fs::read_to_string(&path).unwrap();
                assert!(seen == a || seen == b, "a partial token was read: {seen:?}");
            }
        });
    });
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the token stays private");
    assert!(
        !dir.path().join("control.token.tmp").exists(),
        "no temporary file is left"
    );
}
