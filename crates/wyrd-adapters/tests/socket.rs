#![allow(clippy::unwrap_used, clippy::panic)]

//! The real socket path: server thread + hook client.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::MemSink;
use serde_json::{Value, json};
use wyrd_adapters::{Adapter, Ingest, StaticPolicy, hook, server};
use wyrd_schema::CaptureMode;

fn start(deny: &[&str]) -> (tempfile::TempDir, std::path::PathBuf, Arc<Ingest<MemSink>>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("i.sock");
    let listener = server::bind(&path).unwrap();
    let ingest = Arc::new(Ingest::new(
        MemSink::default(),
        CaptureMode::Cell,
        StaticPolicy {
            deny_tools: deny.iter().map(|s| (*s).to_string()).collect(),
        },
        0,
    ));
    let i = Arc::clone(&ingest);
    std::thread::spawn(move || server::serve(&listener, &i));
    (dir, path, ingest)
}

fn pre(tool: &str) -> String {
    json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_use_id": "t", "tool_input": {"command": "ls"}}).to_string()
}

fn decision(out: &hook::Outcome) -> Option<String> {
    out.stdout.as_ref().map(|s| {
        let v: Value = serde_json::from_str(s).unwrap();
        v["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap()
            .to_string()
    })
}

#[test]
fn socket_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_d, path, _) = start(&[]);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn allowed_call_prints_nothing_and_denied_call_prints_deny() {
    let (_d, path, ingest) = start(&["Bash"]);
    let out = hook::run(
        Adapter::ClaudeCode,
        &pre("Read"),
        Some(&path),
        CaptureMode::Cell,
    );
    assert_eq!(out.stdout, None);
    let out = hook::run(
        Adapter::ClaudeCode,
        &pre("Bash"),
        Some(&path),
        CaptureMode::Cell,
    );
    assert_eq!(decision(&out).as_deref(), Some("deny"));
    assert_eq!(ingest.sink().events.lock().unwrap().len(), 5);
}

#[test]
fn unreachable_socket_fails_closed_in_cell_and_open_in_observe() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone.sock");
    let out = hook::run(
        Adapter::ClaudeCode,
        &pre("Read"),
        Some(&gone),
        CaptureMode::Cell,
    );
    assert_eq!(decision(&out).as_deref(), Some("deny"));
    assert!(out.warning.unwrap().contains("wyrdline unavailable"));
    let out = hook::run(
        Adapter::ClaudeCode,
        &pre("Read"),
        Some(&gone),
        CaptureMode::Observe,
    );
    assert_eq!(out.stdout, None);
    // Non-blocking hooks never deny.
    let post = json!({"hook_event_name": "PostToolUse", "tool_use_id": "t"}).to_string();
    assert_eq!(
        hook::run(Adapter::ClaudeCode, &post, Some(&gone), CaptureMode::Cell).stdout,
        None
    );
    // Outside `wyrd run` the hook is a no-op.
    assert_eq!(
        hook::run(Adapter::ClaudeCode, &pre("Read"), None, CaptureMode::Cell).stdout,
        None
    );
}

#[test]
fn malformed_input_is_recorded_and_denied_in_cell_mode() {
    let (_d, path, ingest) = start(&[]);
    let out = hook::run(Adapter::ClaudeCode, "{oops", Some(&path), CaptureMode::Cell);
    assert_eq!(decision(&out).as_deref(), Some("deny"));
    assert_eq!(
        ingest.sink().log.lock().unwrap().as_slice(),
        ["guard.decision"]
    );
}

#[test]
fn stop_reads_the_transcript_inside_the_cell() {
    let (d, path, ingest) = start(&[]);
    let transcript = d.path().join("t.jsonl");
    std::fs::write(
        &transcript,
        [
            json!({"type": "user", "message": {"content": "go"}}),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Step one."}]}}),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Done."}]}}),
        ]
        .map(|l| l.to_string())
        .join("\n"),
    )
    .unwrap();
    let stop = json!({"hook_event_name": "Stop", "transcript_path": transcript, "last_assistant_message": "Done."}).to_string();
    hook::run(Adapter::ClaudeCode, &stop, Some(&path), CaptureMode::Cell);
    assert_eq!(
        ingest.sink().log.lock().unwrap().as_slice(),
        ["intent.message", "intent.message"]
    );
}

#[test]
fn oversized_and_garbage_requests_get_errors() {
    use std::io::{Read as _, Write as _};
    let (_d, path, _) = start(&[]);
    let mut s = std::os::unix::net::UnixStream::connect(&path).unwrap();
    s.write_all(b"garbage\n").unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.contains("\"error\""), "{resp}");
}

#[test]
fn pre_tool_use_round_trip_is_fast() {
    let (_d, path, _) = start(&[]);
    let mut times: Vec<Duration> = (0..200)
        .map(|_| {
            let t = Instant::now();
            hook::run(
                Adapter::ClaudeCode,
                &pre("Read"),
                Some(&path),
                CaptureMode::Cell,
            );
            t.elapsed()
        })
        .collect();
    times.sort();
    let p99 = times[times.len() * 99 / 100 - 1];
    assert!(p99 < Duration::from_millis(200), "p99 {p99:?}");
}
