//! OpenTelemetry generative-AI spans → intent events (plan §15.4).
//!
//! Input is an OTLP/HTTP JSON `ExportTraceServiceRequest`
//! (`resourceSpans[].scopeSpans[].spans[]`). Spans following the generative-AI
//! semantic conventions with `gen_ai.operation.name = execute_tool` become
//! a tool call (`intent.tool_call.started` + `finished`, source `otlp`).
//! Model-call spans (`chat`, `text_completion`, `invoke_agent`, …) are
//! counted but not recorded: event schema v1 has no kind for model usage.
//! The HTTP receiver (and protobuf decoding) arrives with the daemon API in
//! Phase 7; OTLP is never required.

use serde_json::Value;
use wyrd_bundle::redact::{Level, redact_json, redact_str};
use wyrd_schema::{CallStatus, EventBody, IntentSource, ToolCallFinished, ToolCallStarted};

/// Upper bound on spans read from one request.
pub const MAX_SPANS: usize = 10_000;

const SUMMARY_CHARS: usize = 400;

/// Result of mapping one export request.
#[derive(Debug, Default, PartialEq)]
pub struct Mapped {
    pub events: Vec<EventBody>,
    /// Generative-AI spans that are not tool executions (model calls, agents).
    pub skipped_genai: usize,
    /// Spans without generative-AI attributes.
    pub other: usize,
}

/// An OTLP `AnyValue` in JSON encoding, as plain JSON.
fn any_value(v: &Value) -> Value {
    if let Some(s) = v.get("stringValue") {
        return s.clone();
    }
    if let Some(b) = v.get("boolValue") {
        return b.clone();
    }
    if let Some(i) = v.get("intValue") {
        // 64-bit integers are encoded as strings.
        return match i {
            Value::String(s) => s.parse::<i64>().map_or_else(|_| i.clone(), Value::from),
            other => other.clone(),
        };
    }
    if let Some(d) = v.get("doubleValue") {
        return d.clone();
    }
    if let Some(a) = v.pointer("/arrayValue/values").and_then(Value::as_array) {
        return Value::Array(a.iter().map(any_value).collect());
    }
    Value::Null
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    span.get("attributes")?
        .as_array()?
        .iter()
        .find(|a| a.get("key").and_then(Value::as_str) == Some(key))?
        .get("value")
}

fn attr_str(span: &Value, key: &str) -> Option<String> {
    attr(span, key).map(any_value).and_then(|v| match v {
        Value::String(s) => Some(s),
        Value::Null => None,
        other => Some(other.to_string()),
    })
}

fn nanos(span: &Value, key: &str) -> u64 {
    match span.get(key) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(v) => v.as_u64().unwrap_or(0),
        None => 0,
    }
}

/// OTLP status code 2 (`STATUS_CODE_ERROR`) marks a failed span.
fn is_error(span: &Value) -> bool {
    let status = match span.pointer("/status/code") {
        Some(Value::Number(n)) => n.as_u64() == Some(2),
        Some(Value::String(s)) => s == "STATUS_CODE_ERROR",
        _ => false,
    };
    status || attr(span, "error.type").is_some()
}

fn spans(req: &Value) -> impl Iterator<Item = &Value> {
    let list = |v: &Value, k: &str| -> Vec<Value> {
        v.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let _ = list;
    req.get("resourceSpans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|rs| {
            rs.get("scopeSpans")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .flat_map(|ss| {
            ss.get("spans")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .take(MAX_SPANS)
}

/// Map an OTLP/HTTP JSON trace export to intent events (redacted at
/// `level`), in span start order.
pub fn map_traces(req: &Value, level: Level) -> Mapped {
    let mut out = Mapped::default();
    let mut tools: Vec<(u64, Vec<EventBody>)> = Vec::new();
    for span in spans(req) {
        let Some(op) = attr_str(span, "gen_ai.operation.name") else {
            out.other += 1;
            continue;
        };
        if op != "execute_tool" {
            out.skipped_genai += 1;
            continue;
        }
        let call_id = attr_str(span, "gen_ai.tool.call.id")
            .or_else(|| {
                span.get("spanId")
                    .and_then(Value::as_str)
                    .map(|s| format!("span-{s}"))
            })
            .unwrap_or_else(|| "span-unknown".into());
        let tool = attr_str(span, "gen_ai.tool.name").unwrap_or_else(|| "unknown".into());
        let args = attr_str(span, "gen_ai.tool.call.arguments").map_or(Value::Null, |a| {
            serde_json::from_str(&a).unwrap_or(Value::String(a))
        });
        let start = nanos(span, "startTimeUnixNano");
        let end = nanos(span, "endTimeUnixNano");
        let failed = is_error(span);
        let summary = attr_str(span, "gen_ai.tool.call.result")
            .or_else(|| {
                span.pointer("/status/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .map(|s| redact_str(&s.chars().take(SUMMARY_CHARS).collect::<String>(), level));
        tools.push((
            start,
            vec![
                EventBody::ToolCallStarted(ToolCallStarted {
                    call_id: call_id.clone(),
                    source: IntentSource::Otlp,
                    tool,
                    args: redact_json(&args, level),
                    turn: None,
                }),
                EventBody::ToolCallFinished(ToolCallFinished {
                    call_id,
                    status: if failed {
                        CallStatus::Error
                    } else {
                        CallStatus::Ok
                    },
                    exit_code: None,
                    summary,
                    duration_ms: end.saturating_sub(start) / 1_000_000,
                }),
            ],
        ));
    }
    tools.sort_by_key(|(start, _)| *start);
    out.events = tools.into_iter().flat_map(|(_, e)| e).collect();
    out
}
