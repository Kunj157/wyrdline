//! The MCP server protocol: newline-delimited JSON-RPC 2.0 on stdio
//! (`initialize`, `ping`, `tools/list`, `tools/call`). Every input line is
//! untrusted; nothing here panics on it. Protocol mistakes are JSON-RPC
//! errors; a tool that fails (unknown checkpoint, unknown seq, no running
//! session) answers a result with `isError: true`.

use std::io::{BufRead, Read, Write};

use serde_json::{Map, Value, json};
use wyrd_adapters::SessionQuery;

/// Protocol revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Longest input line accepted (a request is a few hundred bytes).
const MAX_LINE: usize = 1 << 20;

/// Where tool queries are answered.
pub trait Backend {
    fn query(&self, q: &SessionQuery) -> Result<Value, String>;
}

const INSTRUCTIONS: &str = "Wyrdline records this coding session: tool calls, the kernel effects \
they caused, and checkpoints of the workspace. Use these tools to inspect the session. You \
cannot rewind, apply, or change policy yourself: request_rewind only asks a human, who decides.";

fn checkpoint_id_schema() -> Value {
    json!({
        "type": "string",
        "description": "Checkpoint id as listed by list_checkpoints, e.g. c3.",
        "pattern": "^[A-Za-z0-9_-]{1,64}$"
    })
}

fn annotations(title: &str, read_only: bool) -> Value {
    json!({
        "title": title,
        "readOnlyHint": read_only,
        "destructiveHint": false,
        "idempotentHint": read_only,
        "openWorldHint": false
    })
}

/// The tool catalogue (`tools/list`).
pub fn tools() -> Value {
    json!({ "tools": [
        {
            "name": "list_checkpoints",
            "description": "List this session's branches and workspace checkpoints (id, branch, \
                ledger seq, files). Read-only.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": annotations("List checkpoints", true)
        },
        {
            "name": "show_changes_since",
            "description": "List the files that changed from a checkpoint to the latest \
                checkpoint of the current branch (added, removed, modified). Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": { "checkpoint_id": checkpoint_id_schema() },
                "required": ["checkpoint_id"],
                "additionalProperties": false
            },
            "annotations": annotations("Show changes since a checkpoint", true)
        },
        {
            "name": "explain_effect",
            "description": "Explain one recorded kernel effect by its ledger seq: what it did, \
                the process chain that did it, and the tool call it is attributed to, with \
                confidence. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": { "seq": {
                    "type": "integer", "minimum": 1,
                    "description": "Ledger sequence number of the effect."
                } },
                "required": ["seq"],
                "additionalProperties": false
            },
            "annotations": annotations("Explain an effect", true)
        },
        {
            "name": "list_divergences",
            "description": "List divergences: places where what was claimed in the session and \
                what the kernel recorded disagree, with evidence seqs and triage state. \
                Read-only.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": annotations("List divergences", true)
        },
        {
            "name": "request_rewind",
            "description": "Ask the human to rewind this session to a checkpoint. This does not \
                rewind anything: it records a request that stays pending until a human \
                confirms or rejects it in the wyrd CLI or Studio. A confirmed rewind stops \
                the agent and restores the checkpoint on a new branch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "checkpoint_id": checkpoint_id_schema(),
                    "reason": {
                        "type": "string", "minLength": 1, "maxLength": 2000,
                        "description": "Why the rewind is needed, for the human deciding."
                    }
                },
                "required": ["checkpoint_id", "reason"],
                "additionalProperties": false
            },
            "annotations": annotations("Request a rewind (human-confirmed)", false)
        }
    ]})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn ok(id: &Value, result: Value) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), "2.0".into());
    m.insert("id".into(), id.clone());
    m.insert("result".into(), result);
    Value::Object(m)
}

/// The revision to answer `initialize` with: the client's when we speak
/// it, else our newest.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0])
}

fn initialize(params: &Value) -> Value {
    let version = negotiate(params.get("protocolVersion").and_then(Value::as_str));
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "wyrdline", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS
    })
}

fn string_arg(args: &Map<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(_) => Err(format!("argument {name} must be a non-empty string")),
        None => Err(format!("missing argument {name}")),
    }
}

/// Turn a `tools/call` into a query; `Err` is an invalid-params message.
fn parse_call(params: &Value) -> Result<SessionQuery, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call needs a tool name")?;
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err("arguments must be an object".into()),
    };
    match name {
        "list_checkpoints" => Ok(SessionQuery::ListCheckpoints),
        "list_divergences" => Ok(SessionQuery::ListDivergences),
        "show_changes_since" => Ok(SessionQuery::ChangesSince {
            checkpoint_id: string_arg(args, "checkpoint_id")?,
        }),
        "explain_effect" => match args.get("seq").and_then(Value::as_u64) {
            Some(seq) if seq >= 1 => Ok(SessionQuery::ExplainEffect { seq }),
            _ => Err("argument seq must be a positive integer".into()),
        },
        "request_rewind" => Ok(SessionQuery::RequestRewind {
            checkpoint_id: string_arg(args, "checkpoint_id")?,
            reason: string_arg(args, "reason")?,
        }),
        other => Err(format!("unknown tool {other}")),
    }
}

/// The text shown to the model: a rewind request leads with its status.
fn text_of(q: &SessionQuery, v: &Value) -> String {
    let body = serde_json::to_string_pretty(v).unwrap_or_default();
    match (q, v.get("message").and_then(Value::as_str)) {
        (SessionQuery::RequestRewind { .. }, Some(m)) => {
            format!("{}: {m}\n\n{body}", super::views::PENDING)
        }
        _ => body,
    }
}

fn call_tool(backend: &dyn Backend, params: &Value) -> Result<Value, String> {
    let q = parse_call(params)?;
    Ok(match backend.query(&q) {
        Ok(v) => json!({
            "content": [{ "type": "text", "text": text_of(&q, &v) }],
            "structuredContent": v,
            "isError": false
        }),
        Err(message) => json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }),
    })
}

/// Answer one JSON-RPC message; `None` for notifications and responses.
pub fn handle(backend: &dyn Backend, msg: &Value) -> Option<Value> {
    let Some(obj) = msg.as_object() else {
        return Some(error(
            &Value::Null,
            INVALID_REQUEST,
            "not a JSON-RPC object",
        ));
    };
    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        // A response (to nothing we sent) or garbage: only garbage with an
        // id is answered.
        let is_response = obj.contains_key("result") || obj.contains_key("error");
        return match id {
            Some(id) if !is_response => {
                Some(error(&id, INVALID_REQUEST, "request without a method"))
            }
            _ => None,
        };
    };
    let id = id?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error(&id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
    }
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => ok(&id, initialize(&params)),
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, tools()),
        "tools/call" => match call_tool(backend, &params) {
            Ok(result) => ok(&id, result),
            Err(why) => error(&id, INVALID_PARAMS, &why),
        },
        other => error(&id, METHOD_NOT_FOUND, &format!("method not found: {other}")),
    })
}

/// Serve `input` until it closes. A batch (JSON array) is answered with
/// an array of the answers it needs.
pub fn serve(
    backend: &dyn Backend,
    mut input: impl BufRead,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        input
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if buf.is_empty() {
            return Ok(());
        }
        if buf.len() > MAX_LINE {
            // Drop the rest of the oversized line, a bounded chunk at a time.
            while !buf.ends_with(b"\n") {
                buf.clear();
                if input
                    .by_ref()
                    .take(MAX_LINE as u64)
                    .read_until(b'\n', &mut buf)?
                    == 0
                {
                    break;
                }
            }
            write_line(
                out,
                &error(&Value::Null, INVALID_REQUEST, "message too long"),
            )?;
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Err(_) => Some(error(&Value::Null, PARSE_ERROR, "parse error")),
            Ok(Value::Array(items)) if items.is_empty() => {
                Some(error(&Value::Null, INVALID_REQUEST, "empty batch"))
            }
            Ok(Value::Array(items)) => {
                let answers: Vec<Value> = items.iter().filter_map(|m| handle(backend, m)).collect();
                (!answers.is_empty()).then_some(Value::Array(answers))
            }
            Ok(msg) => handle(backend, &msg),
        };
        if let Some(r) = reply {
            write_line(out, &r)?;
        }
    }
}

fn write_line(out: &mut impl Write, v: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, v)?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Backend for Echo {
        fn query(&self, q: &SessionQuery) -> Result<Value, String> {
            match q {
                SessionQuery::ExplainEffect { seq: 404 } => Err("no such effect".into()),
                q => serde_json::to_value(q).map_err(|e| e.to_string()),
            }
        }
    }

    fn run(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(&Echo, input.as_bytes(), &mut out).unwrap_or_default();
        String::from_utf8(out)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn versions_are_negotiated() {
        assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate(Some("1999-01-01")), "2025-06-18");
        assert_eq!(negotiate(None), "2025-06-18");
    }

    #[test]
    fn batches_and_notifications() {
        let out = run(concat!(
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}]"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"result":{}}"#,
            "\n[]\n",
            r#"{"jsonrpc":"1.0","id":3,"method":"ping"}"#,
            "\n"
        ));
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0][0]["id"], 1);
        assert_eq!(out[1]["error"]["code"], INVALID_REQUEST);
        assert_eq!(out[2]["error"]["code"], INVALID_REQUEST);
        assert_eq!(out[2]["id"], 3);
    }

    #[test]
    fn tool_failures_are_results_and_bad_arguments_are_errors() {
        let out = run(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"explain_effect","arguments":{"seq":404}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"explain_effect","arguments":{"seq":0}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_checkpoints","arguments":[1]}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"request_rewind","arguments":{"checkpoint_id":"c1","reason":""}}}"#,
            "\n"
        ));
        assert_eq!(out[0]["result"]["isError"], true);
        assert_eq!(out[0]["result"]["content"][0]["text"], "no such effect");
        for v in &out[1..] {
            assert_eq!(v["error"]["code"], INVALID_PARAMS, "{v}");
        }
    }

    #[test]
    fn oversized_lines_are_refused_and_serving_continues() {
        let big = format!("{}\n", "x".repeat(MAX_LINE + 10));
        let input = format!("{big}{}\n", r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#);
        let out = run(&input);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(out[1]["id"], 5);
    }

    #[test]
    fn every_tool_parses_from_its_schema_example() {
        let t = tools();
        for tool in t["tools"].as_array().into_iter().flatten() {
            let name = tool["name"].as_str().unwrap_or_default();
            let args = match name {
                "show_changes_since" => json!({"checkpoint_id": "c1"}),
                "explain_effect" => json!({"seq": 3}),
                "request_rewind" => json!({"checkpoint_id": "c1", "reason": "why"}),
                _ => json!({}),
            };
            assert!(
                parse_call(&json!({"name": name, "arguments": args})).is_ok(),
                "{name}"
            );
        }
    }
}
