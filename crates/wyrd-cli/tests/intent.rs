#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Intent capture through the real `wyrd hook` path, in observe mode (no
//! privileges needed). The Cell variant lives in `privileged.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use wyrd_schema::{Decision, EventBody};

struct Env {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    data: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    fs::create_dir_all(&ws).unwrap();
    Env { tmp, ws, data }
}

fn wyrd(e: &Env, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(args)
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
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

/// A shell "agent" that drives the hook command like Claude Code does.
fn agent(e: &Env, name: &str, body: &str) -> String {
    let p = e.tmp.path().join(name);
    fs::write(
        &p,
        format!(
            "#!/bin/sh\nset -u\nhook() {{ printf '%s' \"$1\" | \"$WYRD_BIN\" hook claude-code; }}\n{body}\n"
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&p).status().unwrap();
    p.display().to_string()
}

fn session_events(e: &Env) -> Vec<EventBody> {
    let sessions = e.data.join("sessions");
    let sid = fs::read_dir(&sessions)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let ledger = wyrd_ledger::Ledger::open_read_only(sid.join("ledger.wyrdledger")).unwrap();
    ledger
        .events_after(0, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|ev| ev.body)
        .collect()
}

fn kinds(events: &[EventBody]) -> Vec<&'static str> {
    events.iter().map(EventBody::kind).collect()
}

#[test]
fn hooks_become_ledger_events_and_denials_reach_the_agent() {
    let e = env();
    let out_file = e.tmp.path().join("decision.json");
    let script = agent(
        &e,
        "agent.sh",
        &format!(
            r#"hook '{{"hook_event_name":"UserPromptSubmit","prompt":"tidy up"}}'
hook '{{"hook_event_name":"PreToolUse","tool_name":"Read","tool_use_id":"t1","tool_input":{{"file_path":"a"}}}}'
hook '{{"hook_event_name":"PostToolUse","tool_name":"Read","tool_use_id":"t1","tool_response":{{}},"duration_ms":3}}'
hook '{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"t2","tool_input":{{"command":"rm -rf /"}}}}' > {}
hook '{{"hook_event_name":"Stop","last_assistant_message":"Tidied."}}'"#,
            out_file.display()
        ),
    );
    let out = wyrd(
        &e,
        &[
            "run",
            "--observe",
            "--agent",
            "claude-code",
            "--deny-tool",
            "Bash",
            "--",
            &script,
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let decision: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_file).unwrap()).unwrap();
    assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");

    let events = session_events(&e);
    assert_eq!(
        kinds(&events),
        [
            "meta.session.started",
            "meta.session.state_changed",
            "intent.message",
            "intent.tool_call.started",
            "guard.decision",
            "intent.tool_call.finished",
            "intent.tool_call.started",
            "guard.decision",
            "intent.tool_call.denied",
            "intent.message",
            "meta.session.state_changed",
        ]
    );
    let EventBody::GuardDecision(g) = &events[7] else {
        panic!()
    };
    assert_eq!(g.decision, Decision::Deny);
    let EventBody::Message(m) = &events[9] else {
        panic!()
    };
    assert_eq!((m.turn, m.text.as_str()), (1, "Tidied."));
}

#[test]
fn claude_is_detected_and_gets_hook_settings_without_touching_user_files() {
    let e = env();
    let bin = e.tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let args_file = e.tmp.path().join("args.txt");
    let claude = bin.join("claude");
    fs::write(
        &claude,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > {}\n",
            args_file.display()
        ),
    )
    .unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&claude)
        .status()
        .unwrap();
    let out = wyrd(
        &e,
        &[
            "run",
            "--observe",
            "--",
            claude.to_str().unwrap(),
            "-p",
            "hi",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let args: Vec<String> = fs::read_to_string(&args_file)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(args[0], "--settings");
    assert_eq!(&args[2..], ["-p", "hi"]);
    let settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&args[1]).unwrap()).unwrap();
    let cmd = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(cmd.ends_with(" hook claude-code"), "{cmd}");
    assert!(
        Path::new(&args[1]).starts_with(&e.data),
        "settings live in the session dir"
    );
    let EventBody::SessionStarted(s) = &session_events(&e)[0] else {
        panic!()
    };
    assert_eq!(s.agent_kind, "claude-code");
    assert_eq!(
        s.command,
        [claude.to_str().unwrap(), "-p", "hi"],
        "the ledger keeps the user's command"
    );
}

#[test]
fn hook_outside_a_session_is_a_silent_no_op() {
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["hook", "claude-code"])
        .env_remove("WYRD_INTENT_SOCKET")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write as _;
            c.stdin
                .take()
                .unwrap()
                .write_all(br#"{"hook_event_name":"PreToolUse"}"#)?;
            c.wait_with_output()
        })
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"");
}

#[test]
fn hook_fails_closed_when_the_session_socket_is_gone() {
    let e = env();
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["hook", "claude-code"])
        .env("WYRD_INTENT_SOCKET", e.tmp.path().join("missing.sock"))
        .env("WYRD_MODE", "cell")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write as _;
            c.stdin
                .take()
                .unwrap()
                .write_all(br#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#)?;
            c.wait_with_output()
        })
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(String::from_utf8_lossy(&out.stderr).contains("wyrdline unavailable"));
}
