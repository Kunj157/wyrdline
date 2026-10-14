#![allow(clippy::unwrap_used, clippy::panic, clippy::needless_pass_by_value)]

use serde_json::{Value, json};
use wyrd_bundle::redact::Level;
use wyrd_otlp::map_traces;
use wyrd_schema::{CallStatus, EventBody, IntentSource};

fn kv(key: &str, value: Value) -> Value {
    json!({"key": key, "value": value})
}

fn span(id: &str, start: u64, attrs: Vec<Value>, status: u64) -> Value {
    json!({
        "traceId": "5b8efff798038103d269b633813fc60c", "spanId": id, "name": "s",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 42_000_000).to_string(),
        "attributes": attrs, "status": {"code": status}
    })
}

fn export(spans: Vec<Value>) -> Value {
    json!({"resourceSpans": [{"resource": {}, "scopeSpans": [{"scope": {"name": "agent"}, "spans": spans}]}]})
}

#[test]
fn tool_spans_become_tool_calls_in_start_order() {
    let req = export(vec![
        span(
            "b2",
            2_000,
            vec![
                kv(
                    "gen_ai.operation.name",
                    json!({"stringValue": "execute_tool"}),
                ),
                kv("gen_ai.tool.name", json!({"stringValue": "write_file"})),
            ],
            2,
        ),
        span(
            "a1",
            1_000,
            vec![
                kv(
                    "gen_ai.operation.name",
                    json!({"stringValue": "execute_tool"}),
                ),
                kv("gen_ai.tool.name", json!({"stringValue": "shell"})),
                kv("gen_ai.tool.call.id", json!({"stringValue": "call_7"})),
                kv(
                    "gen_ai.tool.call.arguments",
                    json!({"stringValue": "{\"command\":\"ls\"}"}),
                ),
                kv("gen_ai.tool.call.result", json!({"stringValue": "a.txt"})),
            ],
            1,
        ),
        span(
            "c3",
            500,
            vec![
                kv("gen_ai.operation.name", json!({"stringValue": "chat"})),
                kv("gen_ai.usage.input_tokens", json!({"intValue": "120"})),
            ],
            0,
        ),
        span(
            "d4",
            600,
            vec![kv("http.method", json!({"stringValue": "GET"}))],
            0,
        ),
    ]);
    let m = map_traces(&req, Level::Standard);
    assert_eq!((m.skipped_genai, m.other), (1, 1));
    assert_eq!(m.events.len(), 4);
    let EventBody::ToolCallStarted(first) = &m.events[0] else {
        panic!()
    };
    assert_eq!(
        (first.call_id.as_str(), first.tool.as_str(), first.source),
        ("call_7", "shell", IntentSource::Otlp)
    );
    assert_eq!(first.args, json!({"command": "ls"}));
    let EventBody::ToolCallFinished(f) = &m.events[1] else {
        panic!()
    };
    assert_eq!(
        (f.status, f.duration_ms, f.summary.as_deref()),
        (CallStatus::Ok, 42, Some("a.txt"))
    );
    let EventBody::ToolCallStarted(second) = &m.events[2] else {
        panic!()
    };
    assert_eq!(second.call_id, "span-b2", "falls back to the span id");
    let EventBody::ToolCallFinished(f) = &m.events[3] else {
        panic!()
    };
    assert_eq!(f.status, CallStatus::Error);
}

#[test]
fn arguments_are_redacted() {
    // Built at runtime: no secret-shaped literal in the repository.
    let secret = format!("{}{}", "sk-ant-api03-", "aB3dE9fG2h".repeat(4));
    let req = export(vec![span(
        "e5",
        1,
        vec![
            kv(
                "gen_ai.operation.name",
                json!({"stringValue": "execute_tool"}),
            ),
            kv(
                "gen_ai.tool.call.arguments",
                json!({"stringValue": format!("{{\"key\":\"{secret}\"}}")}),
            ),
        ],
        1,
    )]);
    let dump = format!("{:?}", map_traces(&req, Level::Standard).events);
    assert!(!dump.contains(&secret));
}

#[test]
fn malformed_input_maps_to_nothing() {
    for req in [
        json!(null),
        json!({"resourceSpans": 3}),
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [7]}]}]}),
    ] {
        let m = map_traces(&req, Level::Standard);
        assert!(m.events.is_empty(), "{req}");
    }
}
