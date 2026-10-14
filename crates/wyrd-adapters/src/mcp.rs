//! MCP traffic seen by the gateway → intent events.
//!
//! Tracks requests in flight per server to pair responses with their
//! method, maps `tools/call` to tool-call events, and pins every tool's
//! description at its first `tools/list` so a later change (a "rug pull":
//! a server swapping a benign description for a malicious one after it
//! was approved) is flagged.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::Value;
use wyrd_bundle::redact::{redact_json, redact_str};
use wyrd_schema::{CallStatus, EventBody, McpRequest, McpResponse, ToolCallFinished};

use crate::ingest::INGEST_LEVEL;
use crate::protocol::Direction;

/// Maximum characters kept from a result summary.
const SUMMARY_CHARS: usize = 400;

/// Policy id of tool-description pinning decisions.
pub const PINNING_POLICY: &str = "wyrdline.mcp-pinning";

/// What one MCP message means for the session.
#[derive(Debug, Default, PartialEq)]
pub struct Observed {
    pub events: Vec<EventBody>,
    /// A `tools/call` that needs a decision before it is forwarded:
    /// `(call_id, ledger tool name, redacted arguments)`.
    pub call: Option<(String, String, Value)>,
    /// A tool call that finished (checkpoint trigger).
    pub finished: Option<String>,
    /// Tools whose description changed since first seen.
    pub rug_pulls: Vec<String>,
}

#[derive(Debug)]
struct InFlight {
    method: String,
    call_id: Option<String>,
    started: Instant,
}

/// Per-session MCP state.
#[derive(Debug, Default)]
pub struct McpState {
    in_flight: HashMap<(String, String), InFlight>,
    pins: HashMap<(String, String), blake3::Hash>,
}

/// Ledger name of an MCP tool, matching the agent's own naming
/// (`mcp__<server>__<tool>`), so one deny rule covers both paths.
pub fn tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Call id of an MCP tool call.
pub fn call_id(server: &str, request_id: &str) -> String {
    format!("mcp:{server}:{request_id}")
}

fn id_of(msg: &Value) -> Option<String> {
    match msg.get("id")? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn text_summary(result: &Value) -> Option<String> {
    let text: String = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| {
        redact_str(
            &text.chars().take(SUMMARY_CHARS).collect::<String>(),
            INGEST_LEVEL,
        )
    })
}

impl McpState {
    /// Interpret one message. Batches (arrays) are observed element-wise
    /// by the caller.
    pub fn observe(
        &mut self,
        server: &str,
        direction: Direction,
        msg: &Value,
        now: Instant,
    ) -> Observed {
        let mut out = Observed::default();
        let id = id_of(msg);
        let method = msg.get("method").and_then(Value::as_str);
        match (method, id) {
            // A request (either way). Notifications carry no id and are not recorded.
            (Some(method), Some(id)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let params = redact_json(&params, INGEST_LEVEL);
                out.events.push(EventBody::McpRequest(McpRequest {
                    server: server.to_string(),
                    request_id: id.clone(),
                    method: method.to_string(),
                    params: params.clone(),
                }));
                let mut call = None;
                if direction == Direction::ToServer && method == "tools/call" {
                    let name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let cid = call_id(server, &id);
                    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                    out.call = Some((cid.clone(), tool_name(server, name), args));
                    call = Some(cid);
                }
                self.in_flight.insert(
                    (server.to_string(), id),
                    InFlight {
                        method: method.to_string(),
                        call_id: call,
                        started: now,
                    },
                );
            }
            // A response: pair it with its request.
            (None, Some(id)) => {
                let Some(req) = self.in_flight.remove(&(server.to_string(), id.clone())) else {
                    return out;
                };
                let error = msg.get("error");
                let result = msg.get("result").unwrap_or(&Value::Null);
                let tool_error = result.get("isError").and_then(Value::as_bool) == Some(true);
                let summary = match error {
                    Some(e) => e.get("message").and_then(Value::as_str).map(|m| {
                        redact_str(
                            &m.chars().take(SUMMARY_CHARS).collect::<String>(),
                            INGEST_LEVEL,
                        )
                    }),
                    None => text_summary(result),
                };
                out.events.push(EventBody::McpResponse(McpResponse {
                    server: server.to_string(),
                    request_id: id,
                    is_error: error.is_some() || tool_error,
                    result_summary: summary.clone(),
                }));
                if req.method == "tools/list" && error.is_none() {
                    out.rug_pulls = self.pin(server, result);
                }
                if let Some(cid) = req.call_id {
                    out.events
                        .push(EventBody::ToolCallFinished(ToolCallFinished {
                            call_id: cid.clone(),
                            status: if error.is_some() || tool_error {
                                CallStatus::Error
                            } else {
                                CallStatus::Ok
                            },
                            exit_code: None,
                            summary,
                            duration_ms: u64::try_from(now.duration_since(req.started).as_millis())
                                .unwrap_or(u64::MAX),
                        }));
                    out.finished = Some(cid);
                }
            }
            _ => {}
        }
        out
    }

    /// Record each tool's description; return tools whose description
    /// differs from the first one seen.
    fn pin(&mut self, server: &str, result: &Value) -> Vec<String> {
        let mut changed = Vec::new();
        for tool in result
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let basis = serde_json::json!({
                "description": tool.get("description"),
                "inputSchema": tool.get("inputSchema"),
            });
            let h = blake3::hash(basis.to_string().as_bytes());
            match self.pins.entry((server.to_string(), name.to_string())) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(h);
                }
                std::collections::hash_map::Entry::Occupied(o) => {
                    if *o.get() != h {
                        changed.push(name.to_string());
                    }
                }
            }
        }
        changed
    }
}
