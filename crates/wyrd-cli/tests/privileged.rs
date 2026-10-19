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
            // Without a sensor the running session is Degraded, not Recording.
            && (v[0]["status"] == "recording" || v[0]["status"] == "degraded")
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

/// `wyrd-sensord` built on demand (it is another package's binary) and run
/// through `sudo -n` on a temporary socket; exits when dropped.
struct Sensord {
    child: std::process::Child,
    socket: PathBuf,
}

impl Drop for Sensord {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn sensord_bin() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--locked", "-p", "wyrd-sensord"])
        .status()
        .unwrap();
    assert!(status.success());
    Path::new(env!("CARGO_BIN_EXE_wyrd")).with_file_name("wyrd-sensord")
}

fn sensord(e: &Env) -> Sensord {
    let socket = e.outside.join("sensord.sock");
    let child = Command::new("sudo")
        .args(["-n", "--"])
        .arg(sensord_bin())
        .args(["--exit-on-stdin-eof", "--socket"])
        .arg(&socket)
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

fn wyrd_with_sensor(e: &Env, socket: &Path, args: &[&str]) -> Output {
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

#[test]
fn privileged_kernel_effects_are_recorded_from_the_first_syscall() {
    use wyrd_schema::EventBody as B;
    let e = env();
    let d = sensord(&e);
    let agent = script(
        &e,
        "agent.sh",
        "echo edit >> a.txt\nmkdir out\nmv b.txt out/c.txt\nrm a.txt\nexit 4",
    );
    let out = wyrd_with_sensor(&e, &d.socket, &["run", "--", &agent]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("warning"),
        "no capture warning expected"
    );
    let sid = only_session(&e);
    let events = ledger_events(&e, &sid);
    let ws = fs::canonicalize(&e.ws).unwrap().display().to_string();
    let effects: Vec<String> = events
        .iter()
        .filter_map(|b| match b {
            B::ProcExec(x) => Some(format!(
                "exec {}",
                x.filename.rsplit('/').next().unwrap_or(&x.filename)
            )),
            B::FileOpenWrite(x) if x.file.in_cell => {
                Some(format!("write {}", x.file.path.trim_start_matches(&ws)))
            }
            B::FileMkdir(x) if x.in_cell => {
                Some(format!("mkdir {}", x.path.trim_start_matches(&ws)))
            }
            B::FileRename(x) => Some(format!(
                "rename {} {}",
                x.from.path.trim_start_matches(&ws),
                x.to.path.trim_start_matches(&ws)
            )),
            B::FileUnlink(x) if x.file.in_cell => {
                Some(format!("unlink {}", x.file.path.trim_start_matches(&ws)))
            }
            B::ProcExit(x) if x.exit_code == 4 => Some("exit 4".into()),
            _ => None,
        })
        .collect();
    // The agent's own exec is the first effect: nothing before the gate leaks in,
    // nothing of the agent is lost.
    assert_eq!(
        effects.first().map(String::as_str),
        Some("exec agent.sh"),
        "{effects:#?}"
    );
    for want in [
        "write /a.txt",
        "exec mkdir",
        "mkdir /out",
        "exec mv",
        "rename /b.txt /out/c.txt",
        "exec rm",
        "unlink /a.txt",
        "exit 4",
    ] {
        assert!(
            effects.iter().any(|x| x == want),
            "missing {want}: {effects:#?}"
        );
    }
    assert!(!events.iter().any(|b| matches!(b, B::SensorLost(_))));
    let ledger = e.data.join("sessions").join(&sid).join("ledger.wyrdledger");
    assert_eq!(
        wyrd(&e, &["verify", ledger.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn privileged_missing_sensor_marks_the_session_degraded() {
    use wyrd_schema::{EventBody as B, SessionState};
    let e = env();
    let agent = script(&e, "agent.sh", "echo x > a.txt");
    let missing = e.outside.join("no-such.sock");
    let out = wyrd_with_sensor(&e, &missing, &["run", "--", &agent]);
    assert_eq!(out.status.code(), Some(0), "the Cell still runs");
    assert!(String::from_utf8_lossy(&out.stderr).contains("wyrd-sensord is not running"));
    let events = ledger_events(&e, &only_session(&e));
    assert!(events.iter().any(|b| matches!(b, B::SensorLost(_))));
    assert!(
        events
            .iter()
            .any(|b| matches!(b, B::SessionStateChanged(s) if s.to == SessionState::Degraded))
    );
    assert!(
        events.iter().any(|b| matches!(b, B::CheckpointCreated(_))),
        "checkpoints still work"
    );
}

#[test]
fn privileged_sensor_loss_mid_run_is_recorded() {
    use wyrd_schema::EventBody as B;
    let e = env();
    let d = sensord(&e);
    let agent = script(&e, "slow.sh", "echo 1 > a.txt; sleep 1.5; echo 2 > b.txt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["run", "--", &agent])
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("WYRD_SENSORD_SOCKET", &d.socket)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    drop(d); // daemon exits
    assert_eq!(child.wait().unwrap().code(), Some(0));
    let events = ledger_events(&e, &only_session(&e));
    assert!(
        events.iter().any(|b| matches!(b, B::FileOpenWrite(_))),
        "effects before the loss are kept"
    );
    assert!(
        events.iter().any(|b| matches!(b, B::SensorLost(_))),
        "{events:#?}"
    );
}

#[test]
fn privileged_sensor_restart_recovers_the_session() {
    use wyrd_schema::{EventBody as B, SessionState};
    let e = env();
    let first = sensord(&e);
    let socket = first.socket.clone();
    let agent = script(
        &e,
        "restart.sh",
        "echo before > a.txt; sleep 6; echo after > after.txt",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["run", "--", &agent])
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("WYRD_SENSORD_SOCKET", &socket)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(600));
    drop(first); // daemon gone
    let second = sensord(&e); // same socket path, fresh daemon
    assert_eq!(second.socket, socket);
    assert_eq!(child.wait().unwrap().code(), Some(0));
    let events = ledger_events(&e, &only_session(&e));
    let pos = |f: &dyn Fn(&B) -> bool| events.iter().position(f);
    let lost = pos(&|b| matches!(b, B::SensorLost(_))).expect("sensor-lost recorded");
    let kinds: Vec<String> = events
        .iter()
        .map(|b| format!("{} {}", b.kind(), serde_json_lite(b)))
        .collect();
    let resumed = pos(&|b| matches!(b, B::SensorResumed(_)))
        .unwrap_or_else(|| panic!("sensor-resumed not recorded: {kinds:#?}"));
    assert!(lost < resumed);
    assert!(events.iter().any(|b| matches!(b, B::SessionStateChanged(s) if s.from == SessionState::Degraded && s.to == SessionState::Recording)));
    let after = pos(&|b| matches!(b, B::FileOpenWrite(f) if f.file.path.ends_with("/after.txt")));
    assert!(
        after.is_some_and(|a| a > resumed),
        "effects after the restart are captured: {events:#?}"
    );
}

fn serde_json_lite(b: &wyrd_schema::EventBody) -> String {
    format!("{b:?}").chars().take(120).collect()
}

#[test]
fn privileged_policy_kernel_rules_are_enforced_by_landlock() {
    let e = env();
    let home = e.outside.join("home");
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::write(home.join(".ssh/id_test"), "secret key\n").unwrap();
    fs::write(home.join("notes"), "fine\n").unwrap();
    let policy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policies/fixtures/hostile.wasm")
        .canonicalize()
        .unwrap();
    // The hostile fixture denies reads of ~/.ssh and allows TCP port 443 only.
    let data = e.data.display();
    let agent = script(
        &e,
        "agent.sh",
        &format!(
            r#"try() {{ name=$1; shift; if "$@" >/dev/null 2>&1; then echo "$name=ok"; else echo "$name=denied"; fi; }}
{{
try ssh cat "$HOME/.ssh/id_test"
try notes cat "$HOME/notes"
try token sh -c 'cat {data}/sessions/*/control.token'
try ledger sh -c 'cat {data}/sessions/*/ledger.wyrdledger'
try own sh -c 'cat {data}/sessions/*/launch.json'
python3 -c "
import socket
for port in (9, 443):
    s = socket.socket()
    s.settimeout(2)
    try:
        s.connect(('127.0.0.1', port))
        print(f'port{{port}}=open')
    except PermissionError:
        print(f'port{{port}}=denied')
    except OSError:
        print(f'port{{port}}=refused')
"
}} > results.txt"#
        ),
    );
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["run", "--policy"])
        .arg(&policy)
        .args(["--", &agent])
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("HOME", &home)
        .output()
        .unwrap();
    eprintln!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0));
    let sid = only_session(&e);
    let results = fs::read_to_string(
        e.data
            .join("sessions")
            .join(&sid)
            .join("branches/a/upper/results.txt"),
    )
    .unwrap();
    let got: Vec<&str> = results.lines().collect();
    assert_eq!(
        got,
        [
            "ssh=denied",
            "notes=ok",
            "token=denied",
            "ledger=denied",
            "own=ok",
            "port9=denied",
            "port443=refused",
        ]
    );
    // The compiled rules are in the ledger as a kernel-stage decision.
    let kernel = ledger_events(&e, &sid).into_iter().find_map(|b| match b {
        wyrd_schema::EventBody::GuardDecision(g) if g.stage == wyrd_schema::GuardStage::Kernel => {
            Some(g)
        }
        _ => None,
    });
    let g = kernel.expect("kernel decision");
    assert_eq!(g.policy_id, "hostile");
    assert!(g.reason.contains("/.ssh"), "{}", g.reason);
    assert!(g.reason.contains("[443]"), "{}", g.reason);
}

#[test]
fn privileged_policy_egress_is_denied_in_the_kernel_and_recorded() {
    use wyrd_schema::EventBody as B;
    let e = env();
    let d = sensord(&e);
    let policy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policies/fixtures/hostile.wasm")
        .canonicalize()
        .unwrap();
    // The hostile fixture allows 10.0.0.0/8 (and example.org) on port 443.
    let agent = script(
        &e,
        "agent.sh",
        r#"python3 -c "
import socket
for host in ('203.0.113.9', '10.255.255.1'):
    s = socket.socket()
    s.settimeout(0.5)
    try:
        s.connect((host, 443))
        r = 'open'
    except PermissionError:
        r = 'denied'
    except OSError:
        r = 'not denied'
    print(host, r)
" > egress.txt"#,
    );
    let policy = policy.display().to_string();
    let out = wyrd_with_sensor(&e, &d.socket, &["run", "--policy", &policy, "--", &agent]);
    assert_eq!(out.status.code(), Some(0));
    let sid = only_session(&e);
    let got = fs::read_to_string(
        e.data
            .join("sessions")
            .join(&sid)
            .join("branches/a/upper/egress.txt"),
    )
    .unwrap();
    assert_eq!(got, "203.0.113.9 denied\n10.255.255.1 not denied\n");
    let events = ledger_events(&e, &sid);
    let connects: Vec<(String, wyrd_schema::NetVerdict)> = events
        .iter()
        .filter_map(|b| match b {
            B::NetConnect(c) if c.port == 443 => Some((c.addr.clone(), c.verdict)),
            _ => None,
        })
        .collect();
    assert_eq!(
        connects,
        [
            ("203.0.113.9".to_string(), wyrd_schema::NetVerdict::Denied),
            ("10.255.255.1".to_string(), wyrd_schema::NetVerdict::Allowed),
        ]
    );
    let denials: Vec<&str> = events
        .iter()
        .filter_map(|b| match b {
            B::GuardDecision(g)
                if g.stage == wyrd_schema::GuardStage::Kernel
                    && g.decision == wyrd_schema::Decision::Deny =>
            {
                Some(g.reason.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(denials.len(), 1, "{denials:?}");
    assert!(denials[0].contains("203.0.113.9"), "{}", denials[0]);
}
