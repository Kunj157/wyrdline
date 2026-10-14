#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use common::MemSink;
use serde_json::json;
use wyrd_adapters::{Ingest, Request, Response, StaticPolicy};
use wyrd_schema::{CaptureMode, Decision, EventBody};

fn hook(payload: serde_json::Value) -> Request {
    Request::Hook {
        adapter: "claude-code".into(),
        payload,
    }
}

fn ingest(mode: CaptureMode, deny: &[&str]) -> Ingest<MemSink> {
    Ingest::new(
        MemSink::default(),
        mode,
        StaticPolicy {
            deny_tools: deny.iter().map(|s| (*s).to_string()).collect(),
        },
        0,
    )
}

#[test]
fn recorded_session_lands_in_order_with_checkpoint_triggers() {
    let i = ingest(CaptureMode::Cell, &[]);
    for (_, v) in common::fixtures() {
        let r = i.handle(hook(v.clone()));
        if v["hook_event_name"] == "PreToolUse" {
            assert_eq!(
                r,
                Response::Decision {
                    decision: Decision::Allow,
                    reason: None
                }
            );
        } else {
            assert_eq!(r, Response::Ok);
        }
    }
    let log = i.sink().log.lock().unwrap().clone();
    let expected = [
        "intent.message",
        "before_tool_call",
        "intent.tool_call.started",
        "guard.decision",
        "before_tool_call",
        "intent.tool_call.started",
        "guard.decision",
        "intent.tool_call.finished",
        "after_tool_call toolu_01NPENVhpmaAoDAGdiPE75qp",
        "before_tool_call",
        "intent.tool_call.started",
        "guard.decision",
        "intent.tool_call.finished",
        "after_tool_call toolu_0114MmKZQ7iX5jM82apPB444",
        "before_tool_call",
        "intent.tool_call.started",
        "guard.decision",
        "before_tool_call",
        "intent.tool_call.started",
        "guard.decision",
        "intent.tool_call.finished",
        "after_tool_call toolu_01SsnSwwqQQTW3xh2LFf13ge",
        "intent.message",
    ];
    assert_eq!(log, expected);
    let events = i.sink().events.lock().unwrap();
    let EventBody::ToolCallStarted(first) = &events[1] else {
        panic!()
    };
    assert_eq!(first.turn, Some(1));
    let EventBody::Message(last) = events.last().unwrap() else {
        panic!()
    };
    assert_eq!((last.turn, last.text.as_str()), (1, "Done."));
}

#[test]
fn static_policy_denies_by_name_and_prefix() {
    let i = ingest(CaptureMode::Cell, &["Bash", "mcp__github__*"]);
    for (tool, denied) in [
        ("Bash", true),
        ("mcp__github__push", true),
        ("Read", false),
        ("Bashful", false),
    ] {
        let r = i.handle(hook(json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_use_id": format!("id-{tool}"), "tool_input": {}})));
        let Response::Decision { decision, reason } = r else {
            panic!("{r:?}")
        };
        assert_eq!(decision == Decision::Deny, denied, "{tool}");
        assert_eq!(reason.is_some(), denied);
    }
    let kinds: Vec<String> = i.sink().log.lock().unwrap().clone();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| *k == "intent.tool_call.denied")
            .count(),
        2
    );
}

#[test]
fn secrets_never_reach_the_sink() {
    let secret = format!("{}{}", "ghp_", "aB3dE9fG2hJ7kL5mN8pQ1rS4tU6vW0xYzZ9a");
    let i = ingest(CaptureMode::Cell, &[]);
    i.handle(hook(
        json!({"hook_event_name": "UserPromptSubmit", "prompt": format!("use token {secret}")}),
    ));
    i.handle(hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": "t1", "tool_input": {"command": format!("curl -H 'Authorization: token {secret}' x")}})));
    i.handle(hook(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "t1", "tool_response": {"stdout": secret.clone()}})));
    i.handle(hook(
        json!({"hook_event_name": "Stop", "last_assistant_message": format!("set {secret}")}),
    ));
    let dump = serde_json::to_string(&*i.sink().events.lock().unwrap()).unwrap();
    assert!(!dump.contains(&secret), "{dump}");
    assert!(dump.contains("[REDACTED:"));
}

#[test]
fn malformed_payload_fails_closed_in_cell_and_open_in_observe() {
    for (mode, want) in [
        (CaptureMode::Cell, Decision::Deny),
        (CaptureMode::Observe, Decision::Allow),
    ] {
        let i = ingest(mode, &[]);
        let r = i.handle(hook(json!("{not json")));
        let Response::Decision { decision, .. } = r else {
            panic!("{r:?}")
        };
        assert_eq!(decision, want);
        let events = i.sink().events.lock().unwrap();
        let EventBody::GuardDecision(g) = &events[0] else {
            panic!()
        };
        assert_eq!(g.policy_id, "wyrdline.ingest");
        assert_eq!(g.decision, want);
    }
}

#[test]
fn unknown_adapter_and_ledger_failure_are_errors() {
    let i = ingest(CaptureMode::Cell, &[]);
    let r = i.handle(Request::Hook {
        adapter: "nope".into(),
        payload: json!({}),
    });
    assert!(matches!(r, Response::Error { .. }));
    let failing = Ingest::new(
        MemSink {
            fail: true,
            ..MemSink::default()
        },
        CaptureMode::Cell,
        StaticPolicy::default(),
        0,
    );
    let r = failing.handle(hook(
        json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {}}),
    ));
    assert!(
        matches!(r, Response::Error { .. }),
        "a pre-check that cannot be recorded is not allowed"
    );
    assert_eq!(i.handle(Request::Ping), Response::Ok);
}

#[test]
fn turns_continue_from_the_ledger() {
    let i = Ingest::new(
        MemSink::default(),
        CaptureMode::Cell,
        StaticPolicy::default(),
        4,
    );
    i.handle(hook(
        json!({"hook_event_name": "UserPromptSubmit", "prompt": "again"}),
    ));
    let events = i.sink().events.lock().unwrap();
    let EventBody::Message(m) = &events[0] else {
        panic!()
    };
    assert_eq!(m.turn, 5);
}
