#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd fork` warns about the network effects after its checkpoint, on
//! stderr, the way `wyrd rewind` does (issue #264).

use std::collections::BTreeMap;
use std::process::{Command, Output};

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{Event, EventBody, NetConnect, NetVerdict};

struct Env {
    tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
}

/// A session with one checkpoint and, after it, `effects` denied connects.
fn env(effects: usize) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into()],
            observe: false,
            name: None,
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    s.checkpoint(None).unwrap();
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    for _ in 0..effects {
        ledger
            .append(vec![Event {
                seq: 0,
                ts_mono_ns: 0,
                ts_wall_ms: 0,
                process: None,
                drops_since_last: 0,
                body: EventBody::NetConnect(NetConnect {
                    family: 4,
                    addr: "203.0.113.50".into(),
                    port: 80,
                    dns_name: None,
                    verdict: NetVerdict::Denied,
                }),
            }])
            .unwrap();
    }
    let id = s.record().id.clone();
    Env { tmp, data, id }
}

fn fork(e: &Env, name: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["fork", &e.id, "c1", "--name", name])
        .env("WYRD_DATA_DIR", e.data.root())
        .env("XDG_STATE_HOME", e.tmp.path().join("state"))
        .output()
        .unwrap()
}

#[test]
fn fork_warns_about_the_effects_after_its_checkpoint() {
    let e = env(1);
    let out = fork(&e, "experiment");
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("wyrd: warning: 1 effect after c1 cannot be undone:"),
        "{stderr}"
    );
    assert!(
        stderr.contains("net.connect 203.0.113.50:80 (denied)"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("now on branch experiment"), "{stdout}");
    assert!(!stdout.contains("warning"), "stdout stays clean: {stdout}");
}

#[test]
fn fork_prints_nothing_without_effects() {
    let e = env(0);
    let out = fork(&e, "experiment");
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("cannot be undone"), "{stderr}");
}

#[test]
fn a_refused_fork_prints_no_warning() {
    let e = env(1);
    assert_eq!(fork(&e, "experiment").status.code(), Some(0));
    let out = fork(&e, "experiment");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!stderr.contains("cannot be undone"), "{stderr}");
}
