//! MCP Streamable HTTP servers behind the stdio gateway.
//!
//! The agent talks stdio to `wyrd mcp-proxy --remote <file>`; this module
//! relays each JSON-RPC message to the remote server as an HTTP POST and
//! relays the answers back, JSON or Server-Sent Events. Recording and
//! policy checks are the gateway's own ([`Gateway::from_client`],
//! [`Gateway::from_server`]), so remote servers get the same treatment as
//! local ones. Server-initiated messages on a standalone GET stream are not
//! supported; servers answer requests on the POST that carried them.

use std::io::{BufRead as _, BufReader, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{FromClient, Gateway, Reporter, write_line};

/// Where the remote server is; read from a 0600 file so credentials never
/// appear in the process list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Remote {
    pub url: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

impl Remote {
    pub fn load(path: &Path) -> std::io::Result<Remote> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// One POST's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Posted {
    pub status: u16,
    /// `Mcp-Session-Id` issued or confirmed by the server.
    pub session_id: Option<String>,
    /// JSON-RPC messages in the answer (each a JSON text).
    pub messages: Vec<String>,
}

/// The HTTP side, abstracted for tests.
pub trait Transport: Send + Sync {
    /// POST one JSON-RPC message (or batch).
    fn post(
        &self,
        body: &str,
        session: Option<&str>,
        version: Option<&str>,
    ) -> Result<Posted, String>;
    /// End the server-side session (best effort).
    fn delete(&self, session: &str);
}

/// Messages in an SSE body: the `data:` lines of each event.
pub fn sse_messages(body: impl Read) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut data = String::new();
    for line in BufReader::new(body).lines() {
        let line = line?;
        if line.is_empty() {
            if !data.is_empty() {
                out.push(std::mem::take(&mut data));
            }
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    if !data.is_empty() {
        out.push(data);
    }
    Ok(out)
}

/// Streamable HTTP over `ureq`.
#[derive(Debug)]
pub struct Http {
    remote: Remote,
    agent: ureq::Agent,
}

impl Http {
    pub fn new(remote: Remote, timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build();
        Http {
            remote,
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Transport for Http {
    fn post(
        &self,
        body: &str,
        session: Option<&str>,
        version: Option<&str>,
    ) -> Result<Posted, String> {
        let mut req = self
            .agent
            .post(&self.remote.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (k, v) in &self.remote.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(s) = session {
            req = req.header("mcp-session-id", s);
        }
        if let Some(v) = version {
            req = req.header("mcp-protocol-version", v);
        }
        let mut res = req.send(body).map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let header = |name: &str| {
            res.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let session_id = header("mcp-session-id");
        let content_type = header("content-type").unwrap_or_default();
        let mut messages = Vec::new();
        if (200..300).contains(&status) {
            let reader = res.body_mut().as_reader();
            if content_type.starts_with("text/event-stream") {
                messages = sse_messages(reader).map_err(|e| e.to_string())?;
            } else {
                let mut text = String::new();
                BufReader::new(reader)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                if !text.trim().is_empty() {
                    messages.push(text);
                }
            }
        }
        Ok(Posted {
            status,
            session_id,
            messages,
        })
    }

    fn delete(&self, session: &str) {
        let mut req = self
            .agent
            .delete(&self.remote.url)
            .header("mcp-session-id", session);
        for (k, v) in &self.remote.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let _ = req.call();
    }
}

/// JSON-RPC errors for every request (message with an id) in `forwarded`.
fn transport_errors(forwarded: &str, why: &str) -> Vec<String> {
    let items = match serde_json::from_str::<Value>(forwarded) {
        Ok(Value::Array(items)) => items,
        Ok(one) => vec![one],
        Err(_) => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|m| m.get("id").filter(|id| !id.is_null()))
        .map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": format!("remote MCP server: {why}") }
            })
            .to_string()
        })
        .collect()
}

/// The protocol version the server chose in an `initialize` result.
fn negotiated_version(message: &str) -> Option<String> {
    let v: Value = serde_json::from_str(message).ok()?;
    v.pointer("/result/protocolVersion")?
        .as_str()
        .map(str::to_string)
}

/// Relay the agent's stdin to the remote server until the agent closes it.
pub fn run_remote<R: Reporter, T: Transport>(
    gateway: &Gateway<R>,
    transport: &T,
    input: impl Read,
    output: Box<dyn Write + Send>,
) -> std::io::Result<()> {
    let out = Mutex::new(output);
    let mut session: Option<String> = None;
    let mut version: Option<String> = None;
    for line in BufReader::new(input).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (forward, reply) = match gateway.from_client(&line) {
            FromClient::Forward(f) => (Some(f), None),
            FromClient::Reply(r) => (None, Some(r)),
            FromClient::Both { forward, reply } => (Some(forward), Some(reply)),
        };
        if let Some(r) = reply {
            write_line(&out, &r)?;
        }
        let Some(f) = forward else { continue };
        let answers = match transport.post(&f, session.as_deref(), version.as_deref()) {
            Ok(p) if (200..300).contains(&p.status) => {
                if p.session_id.is_some() {
                    session = p.session_id;
                }
                p.messages
            }
            Ok(p) => transport_errors(&f, &format!("HTTP {}", p.status)),
            Err(e) => transport_errors(&f, &e),
        };
        for m in answers {
            if version.is_none() {
                version = negotiated_version(&m);
            }
            let relay = gateway.from_server(&m);
            write_line(&out, &relay)?;
        }
    }
    if let Some(s) = session {
        transport.delete(&s);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_bodies_yield_each_event_data() {
        let body = "event: message\ndata: {\"a\":1}\n\n: comment\ndata: {\"b\":\ndata: 2}\n\n";
        let msgs = sse_messages(body.as_bytes()).unwrap_or_default();
        assert_eq!(
            msgs,
            vec!["{\"a\":1}".to_string(), "{\"b\":\n2}".to_string()]
        );
    }

    #[test]
    fn transport_failures_answer_every_request_in_a_batch() {
        let errs = transport_errors(
            r#"[{"jsonrpc":"2.0","id":1,"method":"a"},{"jsonrpc":"2.0","method":"n"},{"jsonrpc":"2.0","id":"x","method":"b"}]"#,
            "HTTP 502",
        );
        assert_eq!(errs.len(), 2, "notifications get no answer");
        assert!(errs[0].contains("\"id\":1") && errs[0].contains("HTTP 502"));
    }

    #[test]
    fn the_protocol_version_comes_from_the_initialize_result() {
        assert_eq!(
            negotiated_version(
                r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-06-18"}}"#
            )
            .as_deref(),
            Some("2025-06-18")
        );
        assert_eq!(
            negotiated_version(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            None
        );
    }
}
