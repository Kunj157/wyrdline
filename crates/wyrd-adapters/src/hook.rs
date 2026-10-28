//! `wyrd hook <adapter>`: the command an agent's hook configuration runs.
//! It forwards the payload to the intent socket and prints the decision.
//!
//! Failure policy (plan §15.2): when the socket cannot be reached or
//! rejects the request, a pre-execution hook is denied in Cell mode
//! ("wyrdline unavailable") and allowed in observe mode. Other hooks never
//! block the agent.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use wyrd_schema::{CaptureMode, Decision};

use crate::protocol::call;
use crate::{Adapter, Request, Response, claude_code};

/// How long the hook waits for the intent socket (it may be taking a
/// checkpoint of the previous call first, or waiting for a human to answer
/// an ask, which times out sooner: `asks::DEFAULT_TIMEOUT`).
pub const TIMEOUT: Duration = Duration::from_secs(45);

/// Most of a malformed payload kept for the record.
const MALFORMED_KEEP: usize = 4096;

/// What the hook prints and how it exits.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub stdout: Option<String>,
    pub exit_code: u8,
    /// Diagnostic for stderr.
    pub warning: Option<String>,
}

fn print(adapter: Adapter, decision: Decision, reason: Option<&str>) -> Option<String> {
    adapter
        .decision_output(decision, reason)
        .map(|v| v.to_string())
}

/// Parse `WYRD_MODE`; anything but `observe` fails closed as a Cell.
pub fn mode_from_env(value: Option<&str>) -> CaptureMode {
    match value {
        Some("observe") => CaptureMode::Observe,
        _ => CaptureMode::Cell,
    }
}

/// Add the turn's assistant texts from the transcript the agent itself
/// names in a `Stop` payload. Runs inside the Cell, as the agent.
fn enrich(adapter: Adapter, payload: &mut Value) {
    if !matches!(adapter, Adapter::ClaudeCode | Adapter::Codex)
        || !matches!(claude_code::event_name(payload), "Stop" | "SubagentStop")
    {
        return;
    }
    let Some(path) = payload.get("transcript_path").and_then(Value::as_str) else {
        return;
    };
    if let Ok(text) = std::fs::read_to_string(path) {
        let texts = claude_code::assistant_texts(&text);
        if !texts.is_empty() {
            payload[claude_code::TEXTS_FIELD] = Value::from(texts);
        }
    }
}

/// Handle one hook invocation.
pub fn run(adapter: Adapter, input: &str, socket: Option<&Path>, mode: CaptureMode) -> Outcome {
    let parsed = serde_json::from_str::<Value>(input).ok();
    // A payload we cannot read might have been a pre-execution check.
    let pre = parsed.as_ref().is_none_or(|p| adapter.is_pre_tool(p));
    let fail = |why: String| {
        let stdout = (pre && mode == CaptureMode::Cell)
            .then(|| print(adapter, Decision::Deny, Some(&why)))
            .flatten();
        Outcome {
            stdout,
            exit_code: 0,
            warning: Some(why),
        }
    };
    let Some(socket) = socket else {
        // Not running under `wyrd run`: nothing to record or enforce.
        return Outcome {
            stdout: None,
            exit_code: 0,
            warning: None,
        };
    };
    let payload = match parsed {
        Some(mut p) => {
            enrich(adapter, &mut p);
            p
        }
        None => Value::String(input.chars().take(MALFORMED_KEEP).collect()),
    };
    let req = Request::Hook {
        adapter: adapter.name().into(),
        payload,
    };
    match call(socket, &req, TIMEOUT) {
        Ok(Response::Decision { decision, reason }) => Outcome {
            stdout: print(adapter, decision, reason.as_deref()),
            exit_code: 0,
            warning: None,
        },
        Ok(Response::Ok) => Outcome {
            stdout: None,
            exit_code: 0,
            warning: None,
        },
        Ok(Response::Error { message }) => fail(format!("wyrdline rejected the hook: {message}")),
        Ok(Response::Asks { .. } | Response::View { .. }) => {
            fail("wyrdline sent an unexpected answer".into())
        }
        Err(e) => fail(format!("wyrdline unavailable: {e}")),
    }
}
