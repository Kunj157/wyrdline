#![cfg(feature = "privileged-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The unix socket admits only the daemon's own UID, even when the file
//! system would let anyone connect (defence in depth behind 0600).

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use std::sync::Arc;

use wyrd_cell::DataDir;
use wyrdd::AppState;
use wyrdd::listen::SameUidListener;

/// Send one GET as `user` through `sudo -n -u`; returns what came back.
/// The daemon may drop a foreign peer before the request is even written,
/// so a failed send (broken pipe, reset) counts as "nothing came back".
fn request_as(user: &str, socket: &std::path::Path) -> String {
    let script = format!(
        "import socket\ns = socket.socket(socket.AF_UNIX)\ns.settimeout(5)\ns.connect({:?})\n\
         try:\n    s.sendall(b'GET /v1/health HTTP/1.1\\r\\nhost: x\\r\\nconnection: close\\r\\n\\r\\n')\n\
         except (BrokenPipeError, ConnectionResetError):\n    raise SystemExit(0)\n\
         out = b''\nwhile True:\n    try:\n        b = s.recv(4096)\n    except Exception:\n        break\n    if not b: break\n    out += b\n\
         print(out.decode(errors='replace'))",
        socket.display().to_string()
    );
    let out = Command::new("sudo")
        .args(["-n", "-u", user, "--", "python3", "-I", "-c", &script])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn privileged_other_users_are_dropped_even_with_open_permissions() {
    let base = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = base.path().join("run/d.sock");
    let listener = SameUidListener::bind(&socket).unwrap();
    // Open the socket to everyone at the file-system level.
    std::fs::set_permissions(
        socket.parent().unwrap(),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666)).unwrap();
    let app = wyrdd::unix_app(Arc::new(AppState::new(
        DataDir::new(base.path().join("data")),
        "/nonexistent".into(),
    )));
    tokio::spawn(axum::serve(listener, app).into_future());

    let s = socket.clone();
    let foreign = tokio::task::spawn_blocking(move || request_as("nobody", &s))
        .await
        .unwrap();
    assert!(
        foreign.trim().is_empty(),
        "another user got an answer: {foreign}"
    );

    let me = std::env::var("USER").unwrap_or_else(|_| "root".into());
    let s = socket.clone();
    let own = tokio::task::spawn_blocking(move || request_as(&me, &s))
        .await
        .unwrap();
    assert!(own.starts_with("HTTP/1.1 200"), "{own}");
}
