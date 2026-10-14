#![allow(clippy::unwrap_used, clippy::panic)]

//! Codex CLI (same hook format as the first adapter) and Gemini CLI.
//! The Gemini payloads below are constructed from its documented hook
//! contract, not recorded from a live run.

mod common;

use common::MemSink;
use serde_json::json;
use wyrd_adapters::{Adapter, HookEvent, Ingest, Request, Response, StaticPolicy};
use wyrd_schema::{CallStatus, CaptureMode, Decision};

#[test]
fn codex_uses_the_same_payloads_and_decisions() {
    for (_, v) in common::fixtures() {
        assert_eq!(Adapter::Codex.parse(&v), Adapter::ClaudeCode.parse(&v));
    }
    let out = Adapter::Codex
        .decision_output(Decision::Deny, Some("no"))
        .unwrap();
    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(Adapter::from_name("codex"), Some(Adapter::Codex));
}

#[test]
fn gemini_events_map_to_intent() {
    let a = Adapter::GeminiCli;
    let before = json!({"hook_event_name": "BeforeTool", "tool_name": "run_shell_command", "tool_input": {"command": "ls"}});
    let after_ok = json!({"hook_event_name": "AfterTool", "tool_name": "run_shell_command", "tool_input": {"command": "ls"},
                          "tool_response": {"llmContent": "a.txt", "returnDisplay": "a.txt"}});
    let after_err = json!({"hook_event_name": "AfterTool", "tool_name": "write_file", "tool_input": {},
                           "tool_response": {"error": "permission denied"}});
    let (
        HookEvent::PreTool {
            call_id: pre_id,
            tool,
            ..
        },
        HookEvent::PostTool {
            call_id: post_id,
            status,
            summary,
            ..
        },
    ) = (a.parse(&before).unwrap(), a.parse(&after_ok).unwrap())
    else {
        panic!("wrong kinds")
    };
    assert_eq!(tool, "run_shell_command");
    assert_eq!(pre_id, post_id, "Before and After pair without a call id");
    assert_eq!(
        (status, summary.as_deref()),
        (CallStatus::Ok, Some("a.txt"))
    );
    assert!(matches!(
        a.parse(&after_err).unwrap(),
        HookEvent::PostTool {
            status: CallStatus::Error,
            ..
        }
    ));
    assert_eq!(
        a.parse(&json!({"hook_event_name": "BeforeAgent", "prompt": "hi"}))
            .unwrap(),
        HookEvent::UserPrompt { text: "hi".into() }
    );
    assert_eq!(
        a.parse(&json!({"hook_event_name": "AfterAgent", "prompt_response": "done"}))
            .unwrap(),
        HookEvent::AssistantMessages {
            texts: vec!["done".into()]
        }
    );
    assert!(a.is_pre_tool(&before));
    assert!(a.parse(&json!([])).is_err());
}

#[test]
fn gemini_denial_uses_its_decision_format() {
    assert_eq!(
        Adapter::GeminiCli.decision_output(Decision::Allow, None),
        None
    );
    let d = Adapter::GeminiCli
        .decision_output(Decision::Deny, Some("blocked"))
        .unwrap();
    assert_eq!(d, json!({"decision": "deny", "reason": "blocked"}));
    let i = Ingest::new(
        MemSink::default(),
        CaptureMode::Cell,
        StaticPolicy {
            deny_tools: vec!["run_shell_command".into()],
        },
        0,
    );
    let r = i.handle(Request::Hook {
        adapter: "gemini-cli".into(),
        payload: json!({"hook_event_name": "BeforeTool", "tool_name": "run_shell_command", "tool_input": {}}),
    });
    assert!(matches!(
        r,
        Response::Decision {
            decision: Decision::Deny,
            ..
        }
    ));
}
