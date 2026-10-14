#![cfg(feature = "privileged-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Real Cells: overlay mounts, Landlock, privilege drop. Run with
//! `make test-integration` (stage two via `sudo -n`). These tests fail —
//! they never skip — when the machine cannot run a Cell.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    data: PathBuf,
    outside: PathBuf,
}

fn env() -> Env {
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

fn wyrd(e: &Env, args: &[&str]) -> Output {
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

fn script(e: &Env, name: &str, body: &str) -> String {
    let p = e.outside.join(name);
    fs::write(&p, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
    Command::new("chmod").arg("+x").arg(&p).status().unwrap();
    p.display().to_string()
}

fn only_session(e: &Env) -> String {
    let out = wyrd(e, &["sessions", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0]["id"].as_str().unwrap().to_string()
}

/// Poll (with a deadline) until the only session's agent is running.
fn wait_until_recording(e: &Env) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
            .args(["sessions", "--json"])
            .env("WYRD_DATA_DIR", &e.data)
            .output()
            .unwrap();
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && v[0]["status"] == "recording"
            && !v[0]["cgroup"].is_null()
        {
            return v[0]["id"].as_str().unwrap().to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the agent never started: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn host_overlays(ws: &Path) -> usize {
    fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .filter(|l| l.contains(&ws.display().to_string()))
        .count()
}

#[test]
fn privileged_cell_isolates_workspace_and_confines_writes() {
    let e = env();
    let target = e.outside.join("escape.txt");
    let agent = script(
        &e,
        "agent.sh",
        &format!(
            "echo edit >> a.txt\nrm b.txt\nmkdir -p out && echo built > out/x.txt\n\
             if echo pwned > {} 2>/dev/null; then echo ESCAPED; else echo DENIED; fi\nid -u",
            target.display()
        ),
    );
    let out = wyrd(&e, &["run", "--", &agent]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("DENIED"),
        "write outside the workspace must be denied"
    );
    assert!(!target.exists());
    let uid = rustix::process::getuid().as_raw().to_string();
    assert!(
        stdout.lines().any(|l| l == uid),
        "agent must run as the invoking user"
    );
    // The real workspace is untouched.
    assert_eq!(fs::read_to_string(e.ws.join("a.txt")).unwrap(), "hello\n");
    assert!(e.ws.join("b.txt").exists());
    assert!(!e.ws.join("out").exists());
    assert_eq!(
        host_overlays(&e.ws),
        0,
        "no overlay may leak into the host namespace"
    );

    let sid = only_session(&e);
    let cps = wyrd(&e, &["checkpoints", &sid, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&cps.stdout).unwrap();
    assert_eq!(v["checkpoints"].as_array().unwrap().len(), 1);

    // Apply brings the agent's changes into the real workspace.
    assert_eq!(wyrd(&e, &["apply", &sid, "head"]).status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(e.ws.join("a.txt")).unwrap(),
        "hello\nedit\n"
    );
    assert!(!e.ws.join("b.txt").exists());
    assert_eq!(
        fs::read_to_string(e.ws.join("out/x.txt")).unwrap(),
        "built\n"
    );

    assert_eq!(wyrd(&e, &["close", &sid]).status.code(), Some(0));
    let ledger = e.data.join("sessions").join(&sid).join("ledger.wyrdledger");
    assert_eq!(
        wyrd(&e, &["verify", ledger.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn privileged_rewind_and_relaunch_on_a_new_branch() {
    let e = env();
    let first = script(&e, "first.sh", "echo one > a.txt");
    assert_eq!(wyrd(&e, &["run", "--", &first]).status.code(), Some(0));
    let sid = only_session(&e);
    // Same command again on a fork of c1: the branch starts from c1's state.
    assert_eq!(
        wyrd(&e, &["fork", &sid, "c1", "--name", "again", "--relaunch"])
            .status
            .code(),
        Some(0)
    );
    let cps = wyrd(&e, &["checkpoints", &sid, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&cps.stdout).unwrap();
    assert_eq!(v["current_branch"], "b");
    assert_eq!(v["checkpoints"].as_array().unwrap().len(), 2);
    let diff = wyrd(&e, &["diff", &sid, "c1", "c2", "--json"]);
    let changes: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(
        changes.as_array().unwrap().len(),
        0,
        "re-running the same agent changes nothing"
    );
    assert_eq!(fs::read_to_string(e.ws.join("a.txt")).unwrap(), "hello\n");
}

#[test]
fn privileged_checkpoint_while_running_freezes_consistently() {
    let e = env();
    let agent = script(
        &e,
        "slow.sh",
        "for i in 1 2 3 4 5 6 7 8 9 10; do echo $i > n.txt; sleep 0.2; done",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["run", "--", &agent])
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .spawn()
        .unwrap();
    let sid = wait_until_recording(&e);
    let cp = wyrd(&e, &["checkpoint", &sid]);
    assert_eq!(cp.status.code(), Some(0));
    assert_eq!(child.wait().unwrap().code(), Some(0));
    let cps = wyrd(&e, &["checkpoints", &sid, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&cps.stdout).unwrap();
    assert_eq!(
        v["checkpoints"].as_array().unwrap().len(),
        2,
        "mid-run + final checkpoint"
    );
    let ledger = e.data.join("sessions").join(&sid).join("ledger.wyrdledger");
    assert_eq!(
        wyrd(&e, &["verify", ledger.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn privileged_stop_kills_the_whole_tree() {
    let e = env();
    let agent = script(&e, "forever.sh", "sleep 300 & sleep 300");
    let mut child = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["run", "--", &agent])
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .spawn()
        .unwrap();
    let sid = wait_until_recording(&e);
    let stop = wyrd(&e, &["stop", &sid]);
    assert_eq!(String::from_utf8_lossy(&stop.stdout).trim(), "stopped");
    let status = child.wait().unwrap();
    assert!(!status.success(), "the agent was killed");
}

fn ledger_events(e: &Env, sid: &str) -> Vec<wyrd_schema::EventBody> {
    let path = e.data.join("sessions").join(sid).join("ledger.wyrdledger");
    wyrd_ledger::Ledger::open_read_only(path)
        .unwrap()
        .events_after(0, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|ev| ev.body)
        .collect()
}

#[test]
fn privileged_every_tool_call_gets_its_own_checkpoint() {
    use wyrd_schema::EventBody;
    let e = env();
    // The Cell may write to /tmp but not to the test's own directory.
    let scratch = tempfile::tempdir().unwrap();
    let decision = scratch.path().join("decision.json");
    let hook = "hook() { printf '%s' \"$1\" | \"$WYRD_BIN\" hook claude-code; }";
    let pre = |id: &str, tool: &str| {
        format!(
            "hook '{{\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"{tool}\",\"tool_use_id\":\"{id}\",\"tool_input\":{{}}}}'"
        )
    };
    let post = |id: &str| {
        format!(
            "hook '{{\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Write\",\"tool_use_id\":\"{id}\",\"tool_response\":{{}}}}'"
        )
    };
    let body = [
        hook.to_string(),
        pre("t1", "Write"),
        "echo one > one.txt".into(),
        post("t1"),
        pre("t2", "Write"),
        "echo two > two.txt && rm b.txt".into(),
        post("t2"),
        format!("{} > {}", pre("t3", "Bash"), decision.display()),
    ]
    .join("\n");
    let agent = script(&e, "hooks.sh", &body);
    let out = wyrd(
        &e,
        &[
            "run",
            "--agent",
            "claude-code",
            "--deny-tool",
            "Bash",
            "--",
            &agent,
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&decision).unwrap()).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");

    let sid = only_session(&e);
    let events = ledger_events(&e, &sid);
    let checkpoints: Vec<Option<String>> = events
        .iter()
        .filter_map(|b| match b {
            EventBody::CheckpointCreated(c) => Some(c.after_call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        checkpoints,
        [Some("t1".into()), Some("t2".into())],
        "one checkpoint per finished call, none extra at exit"
    );
    // Each checkpoint follows its call's finish and precedes the next call.
    let pos = |pred: &dyn Fn(&EventBody) -> bool| events.iter().position(pred).unwrap();
    let c1 = pos(
        &|b| matches!(b, EventBody::CheckpointCreated(c) if c.after_call_id.as_deref() == Some("t1")),
    );
    let t2 = pos(&|b| matches!(b, EventBody::ToolCallStarted(c) if c.call_id == "t2"));
    assert!(c1 < t2);
    // Rewinding to the first call's checkpoint restores exactly its state.
    assert_eq!(wyrd(&e, &["rewind", &sid, "c1"]).status.code(), Some(0));
    let cps = wyrd(&e, &["diff", &sid, "c1", "c2", "--json"]);
    let changes: serde_json::Value = serde_json::from_slice(&cps.stdout).unwrap();
    let paths: Vec<&str> = changes
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["b.txt", "two.txt"]);
    let ledger = e.data.join("sessions").join(&sid).join("ledger.wyrdledger");
    assert_eq!(
        wyrd(&e, &["verify", ledger.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
}
