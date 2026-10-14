//! Gemini CLI hook adapter.
//!
//! Built from Gemini CLI's documented hook contract (not yet verified
//! against a live run — the CLI is not installed on the development
//! machine): `BeforeTool`/`AfterTool` carry `tool_name` and `tool_input`
//! (and `tool_response` after the call), `BeforeAgent` carries the user's
//! `prompt`, `AfterAgent` the final `prompt_response`. A pre-execution
//! decision is returned as `{"decision": "deny", "reason": …}`. Gemini
//! CLI sends no call id, so Before and After are paired by a hash of the
//! tool and its input.

use serde_json::{Value, json};
use wyrd_schema::{CallStatus, Decision};

use crate::HookEvent;
use crate::claude_code::call_id;

/// Adapter name recorded in the ledger and used by `wyrd hook <name>`.
pub const NAME: &str = "gemini-cli";

const SUMMARY_CHARS: usize = 400;

pub fn event_name(payload: &Value) -> &str {
    payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

pub fn parse(payload: &Value) -> Result<HookEvent, String> {
    if !payload.is_object() {
        return Err("hook payload is not a JSON object".into());
    }
    let str_field = |k: &str| payload.get(k).and_then(Value::as_str).unwrap_or("");
    Ok(match event_name(payload) {
        "" => return Err("hook payload has no hook_event_name".into()),
        "BeforeAgent" => HookEvent::UserPrompt {
            text: str_field("prompt").to_string(),
        },
        "BeforeTool" => HookEvent::PreTool {
            call_id: call_id(payload),
            tool: payload
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            args: payload.get("tool_input").cloned().unwrap_or(Value::Null),
        },
        "AfterTool" => {
            let resp = payload.get("tool_response").unwrap_or(&Value::Null);
            let error = resp.get("error").and_then(text);
            let summary = error
                .clone()
                .or_else(|| resp.get("returnDisplay").and_then(text))
                .or_else(|| resp.get("llmContent").and_then(text))
                .map(|s| s.chars().take(SUMMARY_CHARS).collect());
            HookEvent::PostTool {
                call_id: call_id(payload),
                status: if error.is_some() {
                    CallStatus::Error
                } else {
                    CallStatus::Ok
                },
                exit_code: None,
                summary,
                duration_ms: 0,
            }
        }
        "AfterAgent" => HookEvent::AssistantMessages {
            texts: Some(str_field("prompt_response"))
                .filter(|t| !t.trim().is_empty())
                .map(|t| vec![t.to_string()])
                .unwrap_or_default(),
        },
        _ => HookEvent::Ignored,
    })
}

/// Hook stdout for a pre-execution decision; `Allow` defers to the CLI.
pub fn decision_output(decision: Decision, reason: Option<&str>) -> Option<Value> {
    let d = match decision {
        Decision::Allow => return None,
        // Gemini CLI has no "ask" outcome for hooks: deny with the reason.
        Decision::Deny | Decision::Ask => "deny",
    };
    Some(json!({ "decision": d, "reason": reason.unwrap_or("denied by wyrdline") }))
}
