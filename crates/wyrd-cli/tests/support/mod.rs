//! Harness shared by the privileged tests and the escape suite
//! (`tests/security/escape`): temporary workspaces, the `wyrd` binary
//! through the sudo helper, a private `wyrd-sensord`, ledger access.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Env {
    _tmp: tempfile::TempDir,
    pub ws: PathBuf,
    pub data: PathBuf,
    pub outside: PathBuf,
}

pub fn env() -> Env {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&ws).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(ws.join("a.txt"), "hello\n").unwrap();
    fs::write(ws.join("b.txt"), "bye\n").unwrap();
    Env {
        _tmp: tmp,
        ws,
        data,
        outside,
    }
}

pub fn wyrd(e: &Env, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(args)
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .output()
        .unwrap();
    eprintln!(
        "wyrd {args:?} -> {:?}\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

pub fn script(e: &Env, name: &str, body: &str) -> String {
    let p = e.outside.join(name);
    fs::write(&p, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
    Command::new("chmod").arg("+x").arg(&p).status().unwrap();
    p.display().to_string()
}

pub fn only_session(e: &Env) -> String {
    let out = wyrd(e, &["sessions", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0]["id"].as_str().unwrap().to_string()
}

pub fn ledger_events(e: &Env, sid: &str) -> Vec<wyrd_schema::EventBody> {
    let path = e.data.join("sessions").join(sid).join("ledger.wyrdledger");
    wyrd_ledger::Ledger::open_read_only(path)
        .unwrap()
        .events_after(0, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|ev| ev.body)
        .collect()
}

/// `wyrd-sensord` built on demand (it is another package's binary) and run
/// through `sudo -n` on a temporary socket; exits when dropped.
pub struct Sensord {
    pub child: std::process::Child,
    pub socket: PathBuf,
}

impl Drop for Sensord {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

pub fn sensord_bin() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--locked", "-p", "wyrd-sensord"])
        .status()
        .unwrap();
    assert!(status.success());
    Path::new(env!("CARGO_BIN_EXE_wyrd")).with_file_name("wyrd-sensord")
}

pub fn sensord(e: &Env) -> Sensord {
    sensord_with(e, &[])
}

/// As [`sensord`], with extra daemon arguments.
pub fn sensord_with(e: &Env, extra: &[&str]) -> Sensord {
    let socket = e.outside.join("sensord.sock");
    let child = Command::new("sudo")
        .args(["-n", "--"])
        .arg(sensord_bin())
        .args(["--exit-on-stdin-eof", "--socket"])
        .arg(&socket)
        .args(extra)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !socket.exists() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "sensord did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Sensord { child, socket }
}

pub fn wyrd_with_sensor(e: &Env, socket: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(args)
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("WYRD_SENSORD_SOCKET", socket)
        .output()
        .unwrap();
    eprintln!(
        "wyrd {args:?} -> {:?}\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
