#![allow(clippy::unwrap_used, clippy::panic)]

//! Claude Code payloads recorded from a real session (Claude Code 2.1,
//! redacted: session ids and paths replaced).

mod common;

use serde_json::json;
use wyrd_adapters::HookEvent;
use wyrd_adapters::claude_code::{
    assistant_texts, decision_output, inject_settings, parse, settings,
};
use wyrd_schema::{CallStatus, Decision};

#[test]
fn recorded_session_maps_to_events() {
    let events: Vec<(String, HookEvent)> = common::fixtures()
        .into_iter()
        .map(|(name, v)| (name, parse(&v).unwrap()))
        .collect();
    let find = |n: &str| &events.iter().find(|(f, _)| f.starts_with(n)).unwrap().1;
    assert_eq!(find("01-"), &HookEvent::Ignored, "SessionStart");
    assert!(
        matches!(find("02-"), HookEvent::UserPrompt { text } if text.starts_with("Run the shell command"))
    );
    match find("03-") {
        HookEvent::PreTool {
            call_id,
            tool,
            args,
        } => {
            assert_eq!(tool, "Bash");
            assert!(call_id.starts_with("toolu_"));
            assert_eq!(args["command"], "ls");
        }
        other => panic!("{other:?}"),
    }
    match find("05-") {
        HookEvent::PostTool {
            status,
            exit_code,
            summary,
            duration_ms,
            ..
        } => {
            assert_eq!(*status, CallStatus::Ok);
            assert_eq!(*exit_code, None);
            assert_eq!(summary.as_deref(), Some("notes.txt"));
            assert_eq!(*duration_ms, 47);
        }
        other => panic!("{other:?}"),
    }
    match find("07-") {
        HookEvent::PostTool {
            status,
            exit_code,
            summary,
            ..
        } => {
            assert_eq!(*status, CallStatus::Error);
            assert_eq!(*exit_code, Some(1));
            assert_eq!(summary.as_deref(), Some("Exit code 1"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        find("11-"),
        &HookEvent::AssistantMessages {
            texts: vec!["Done.".into()]
        }
    );
    assert_eq!(find("12-"), &HookEvent::Ignored, "SessionEnd");
}

#[test]
fn every_pre_has_the_same_id_as_its_post() {
    let mut started = std::collections::BTreeSet::new();
    for (_, v) in common::fixtures() {
        match parse(&v).unwrap() {
            HookEvent::PreTool { call_id, .. } => {
                started.insert(call_id);
            }
            HookEvent::PostTool { call_id, .. } => assert!(started.contains(&call_id)),
            _ => {}
        }
    }
}

#[test]
fn malformed_payloads_are_errors_not_panics() {
    for v in [
        json!("text"),
        json!([1, 2]),
        json!({}),
        json!({"hook_event_name": 3}),
        json!(null),
    ] {
        assert!(parse(&v).is_err(), "{v}");
    }
    // Unknown future events are ignored rather than rejected.
    assert_eq!(
        parse(&json!({"hook_event_name": "Elicitation"})).unwrap(),
        HookEvent::Ignored
    );
}

#[test]
fn missing_tool_use_id_gets_a_stable_hash() {
    let pre = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}});
    let post = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": {"stdout": ""}});
    let (HookEvent::PreTool { call_id: a, .. }, HookEvent::PostTool { call_id: b, .. }) =
        (parse(&pre).unwrap(), parse(&post).unwrap())
    else {
        panic!("wrong kinds")
    };
    assert_eq!(a, b);
    assert!(a.starts_with("h-"));
}

#[test]
fn allow_defers_to_claude_codes_own_permissions() {
    assert_eq!(decision_output(Decision::Allow, None), None);
    let deny = decision_output(Decision::Deny, Some("no")).unwrap();
    assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(deny["hookSpecificOutput"]["permissionDecisionReason"], "no");
    assert_eq!(deny["hookSpecificOutput"]["hookEventName"], "PreToolUse");
}

#[test]
fn transcript_texts_of_the_last_turn_only() {
    let lines = [
        json!({"type": "user", "message": {"content": "first prompt"}}),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "old turn"}]}}),
        json!({"type": "user", "message": {"content": "second prompt"}}),
        json!({"type": "assistant", "message": {"content": [{"type": "thinking", "thinking": "hidden"}]}}),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Running the tests."}, {"type": "tool_use", "name": "Bash"}]}}),
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "ok"}]}}),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "All tests pass."}]}}),
    ];
    let text = lines
        .iter()
        .map(|l| format!("{l}\nnot json"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        assistant_texts(&text),
        vec!["Running the tests.", "All tests pass."]
    );
}

#[test]
fn transcript_texts_win_over_last_message() {
    let v = json!({"hook_event_name": "Stop", "last_assistant_message": "b", "wyrd_assistant_texts": ["a", "b"]});
    assert_eq!(
        parse(&v).unwrap(),
        HookEvent::AssistantMessages {
            texts: vec!["a".into(), "b".into()]
        }
    );
}

#[test]
fn settings_register_every_hook_and_injection_keeps_arguments() {
    let s = settings(std::path::Path::new("/opt/wyrd"));
    for ev in [
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "UserPromptSubmit",
        "Stop",
        "SessionEnd",
    ] {
        let cmd = s["hooks"][ev][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "/opt/wyrd hook claude-code", "{ev}");
    }
    let cmd: Vec<String> = ["claude", "-p", "hi"].map(String::from).to_vec();
    assert_eq!(
        inject_settings(&cmd, std::path::Path::new("/s.json")),
        ["claude", "--settings", "/s.json", "-p", "hi"]
    );
}
