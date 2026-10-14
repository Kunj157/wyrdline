//! Claude Code hook adapter.
//!
//! Payload shapes were recorded from Claude Code 2.1 (fixtures in
//! `tests/fixtures/claude-code/`): `PreToolUse` and `PostToolUse` carry
//! `tool_name`, `tool_input`, `tool_use_id`; failed calls arrive as
//! `PostToolUseFailure` with `error: "Exit code N"`; `Stop` carries
//! `last_assistant_message`. Decisions go back as
//! `hookSpecificOutput.permissionDecision`.

use std::path::Path;

use serde_json::{Value, json};
use wyrd_schema::{CallStatus, Decision};

use crate::HookEvent;

/// Adapter name recorded in the ledger and used by `wyrd hook <name>`.
pub const NAME: &str = "claude-code";

/// Field the hook client adds to `Stop` payloads with every assistant text
/// of the turn, read from the agent's transcript inside the Cell.
pub const TEXTS_FIELD: &str = "wyrd_assistant_texts";

/// Maximum characters kept from a tool result summary.
const SUMMARY_CHARS: usize = 400;

/// Upper bound on assistant text taken from a transcript per turn.
const MAX_TRANSCRIPT_TEXT: usize = 64 * 1024;

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// The hook event name of a payload.
pub fn event_name(payload: &Value) -> &str {
    str_field(payload, "hook_event_name").unwrap_or("")
}

/// Call id: Claude Code's `tool_use_id`, else a hash of the tool and input
/// (stable between Pre and Post of the same call).
pub fn call_id(payload: &Value) -> String {
    if let Some(id) = str_field(payload, "tool_use_id") {
        return id.to_string();
    }
    let basis = format!(
        "{}{}",
        str_field(payload, "tool_name").unwrap_or(""),
        payload
            .get("tool_input")
            .map(Value::to_string)
            .unwrap_or_default()
    );
    format!(
        "h-{}",
        &hex::encode(blake3::hash(basis.as_bytes()).as_bytes())[..16]
    )
}

/// Exit code reported by a tool: a numeric field, or the `Exit code N`
/// text of a failure.
fn exit_code(payload: &Value) -> Option<i32> {
    let resp = payload.get("tool_response").unwrap_or(&Value::Null);
    let numeric = ["exit_code", "exitCode", "returnCode", "return_code"]
        .iter()
        .find_map(|k| resp.get(*k).and_then(Value::as_i64))
        .and_then(|c| i32::try_from(c).ok());
    numeric.or_else(|| {
        str_field(payload, "error")?
            .strip_prefix("Exit code ")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

fn summary(payload: &Value) -> Option<String> {
    let resp = payload.get("tool_response").unwrap_or(&Value::Null);
    let text = str_field(payload, "error")
        .map(str::to_string)
        .or_else(|| {
            ["stdout", "output", "content", "result"]
                .iter()
                .find_map(|k| resp.get(*k).and_then(Value::as_str).map(str::to_string))
        })
        .or_else(|| resp.as_str().map(str::to_string))?;
    Some(text.chars().take(SUMMARY_CHARS).collect())
}

fn failed(payload: &Value) -> bool {
    let resp = payload.get("tool_response").unwrap_or(&Value::Null);
    event_name(payload) == "PostToolUseFailure"
        || resp.get("is_error").and_then(Value::as_bool) == Some(true)
        || resp.get("success").and_then(Value::as_bool) == Some(false)
        || resp.get("interrupted").and_then(Value::as_bool) == Some(true)
}

/// Translate one hook payload. Unknown events are ignored; a payload
/// without an event name is malformed.
pub fn parse(payload: &Value) -> Result<HookEvent, String> {
    if !payload.is_object() {
        return Err("hook payload is not a JSON object".into());
    }
    Ok(match event_name(payload) {
        "" => return Err("hook payload has no hook_event_name".into()),
        "UserPromptSubmit" => HookEvent::UserPrompt {
            text: str_field(payload, "prompt").unwrap_or("").to_string(),
        },
        "PreToolUse" => HookEvent::PreTool {
            call_id: call_id(payload),
            tool: str_field(payload, "tool_name")
                .unwrap_or("unknown")
                .to_string(),
            args: payload.get("tool_input").cloned().unwrap_or(Value::Null),
        },
        "PostToolUse" | "PostToolUseFailure" => {
            let code = exit_code(payload);
            let status = if failed(payload) || code.is_some_and(|c| c != 0) {
                if payload.get("is_interrupt").and_then(Value::as_bool) == Some(true) {
                    CallStatus::Cancelled
                } else {
                    CallStatus::Error
                }
            } else {
                CallStatus::Ok
            };
            HookEvent::PostTool {
                call_id: call_id(payload),
                status,
                exit_code: code,
                summary: summary(payload),
                duration_ms: payload
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            }
        }
        "Stop" | "SubagentStop" => {
            let from_transcript: Vec<String> = payload
                .get(TEXTS_FIELD)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            // The transcript may not be flushed yet when Stop fires: the
            // final message is then only in `last_assistant_message`.
            let mut texts = from_transcript;
            if let Some(last) = str_field(payload, "last_assistant_message")
                && !last.trim().is_empty()
                && texts.last().map(String::as_str) != Some(last)
            {
                texts.push(last.to_string());
            }
            HookEvent::AssistantMessages { texts }
        }
        _ => HookEvent::Ignored,
    })
}

/// Hook stdout for a pre-execution decision. `Allow` prints nothing so
/// Claude Code's own permission rules still apply: Wyrdline only ever
/// narrows what the agent may do.
pub fn decision_output(decision: Decision, reason: Option<&str>) -> Option<Value> {
    let d = match decision {
        Decision::Allow => return None,
        Decision::Deny => "deny",
        Decision::Ask => "ask",
    };
    let mut out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": d,
        }
    });
    if let Some(r) = reason {
        out["hookSpecificOutput"]["permissionDecisionReason"] = json!(r);
    }
    Some(out)
}

/// Assistant texts of the latest turn from a Claude Code transcript (JSONL):
/// every `assistant` text block after the last user prompt.
pub fn assistant_texts(transcript: &str) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();
    for line in transcript.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let content = v.pointer("/message/content");
        match str_field(&v, "type") {
            // A real prompt has string content; tool results come back as
            // `user` entries with a list of `tool_result` blocks.
            Some("user") if content.is_some_and(Value::is_string) => texts.clear(),
            Some("assistant") => {
                for block in content.and_then(Value::as_array).into_iter().flatten() {
                    if str_field(block, "type") == Some("text")
                        && let Some(t) = str_field(block, "text")
                        && !t.trim().is_empty()
                    {
                        texts.push(t.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let mut budget = MAX_TRANSCRIPT_TEXT;
    texts
        .into_iter()
        .rev()
        .take_while(|t| {
            let ok = t.len() <= budget;
            budget = budget.saturating_sub(t.len());
            ok
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Settings JSON that registers `wyrd hook claude-code` for every event,
/// passed to Claude Code with `--settings` (no user file is modified).
pub fn settings(wyrd: &Path) -> Value {
    let cmd = format!("{} hook {NAME}", wyrd.display());
    let handler = |timeout: u32| json!([{ "type": "command", "command": cmd, "timeout": timeout }]);
    let tools = |timeout: u32| json!([{ "matcher": "*", "hooks": handler(timeout) }]);
    let plain = |timeout: u32| json!([{ "hooks": handler(timeout) }]);
    json!({
        "hooks": {
            "PreToolUse": tools(30),
            "PostToolUse": tools(30),
            "PostToolUseFailure": tools(30),
            "UserPromptSubmit": plain(10),
            "Stop": plain(30),
            "SubagentStop": plain(30),
            "SessionStart": plain(10),
            "SessionEnd": plain(10),
        }
    })
}

/// Insert `--settings <file>` right after the program name.
pub fn inject_settings(command: &[String], settings_file: &Path) -> Vec<String> {
    let mut out = Vec::with_capacity(command.len() + 2);
    if let Some((prog, rest)) = command.split_first() {
        out.push(prog.clone());
        out.push("--settings".into());
        out.push(settings_file.display().to_string());
        out.extend(rest.iter().cloned());
    }
    out
}
