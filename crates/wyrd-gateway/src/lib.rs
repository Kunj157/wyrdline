//! MCP gateway (stdio transport; remote Streamable HTTP servers in
//! [`remote`]).
//!
//! `wyrd mcp-proxy --server <name> -- <command…>` starts the real MCP
//! server as a child and relays newline-delimited JSON-RPC between the
//! agent (our stdin/stdout) and the server. Every message is reported to
//! the session's intent socket; a `tools/call` is forwarded only when the
//! socket allows it, otherwise the agent receives a JSON-RPC error whose
//! `data.wyrdline_reason` says why. The server's stderr passes through and
//! the proxy exits with the server's exit code.
//!
//! When the intent socket cannot be reached, `tools/call` is denied in
//! Cell mode and forwarded in observe mode; other messages always pass.

use std::io::{BufRead as _, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

pub mod remote;
use wyrd_adapters::protocol::call;
use wyrd_adapters::{Direction, Request, Response};
use wyrd_schema::{CaptureMode, Decision};

/// JSON-RPC error code used for policy denials (implementation-defined range).
pub const DENIED_CODE: i64 = -32001;

/// How long to wait for a decision on `tools/call`.
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(45);

/// Where gateway reports go.
pub trait Reporter: Send + Sync {
    /// Report one message; `Err` when the intent socket is unavailable.
    fn report(
        &self,
        server: &str,
        direction: Direction,
        message: &Value,
    ) -> Result<Response, String>;
}

/// Reports to the intent socket.
#[derive(Debug, Clone)]
pub struct SocketReporter {
    pub socket: PathBuf,
}

impl Reporter for SocketReporter {
    fn report(
        &self,
        server: &str,
        direction: Direction,
        message: &Value,
    ) -> Result<Response, String> {
        let req = Request::Mcp {
            server: server.to_string(),
            direction,
            message: message.clone(),
        };
        call(&self.socket, &req, DECISION_TIMEOUT).map_err(|e| e.to_string())
    }
}

/// The proxy's view of one server.
#[derive(Debug)]
pub struct Gateway<R> {
    pub server: String,
    pub mode: CaptureMode,
    /// `None` when not running under `wyrd run`: plain relay.
    pub reporter: Option<R>,
}

/// What to do with a line from the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromClient {
    /// Send this line to the server (possibly a reduced batch).
    Forward(String),
    /// Answer the agent directly with this line.
    Reply(String),
    /// Forward part of a batch and answer the rest.
    Both { forward: String, reply: String },
}

fn denial(id: &Value, reason: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": DENIED_CODE,
            "message": format!("denied by wyrdline: {reason}"),
            "data": { "wyrdline_reason": reason },
        }
    })
}

impl<R: Reporter> Gateway<R> {
    /// Decide on one agent → server message. Returns the denial reason if
    /// it must not be forwarded.
    fn check(&self, msg: &Value) -> Option<String> {
        let is_call = msg.get("method").and_then(Value::as_str) == Some("tools/call");
        let reporter = self.reporter.as_ref()?;
        let fail = |why: String| {
            tracing::warn!(server = %self.server, "{why}");
            (is_call && self.mode == CaptureMode::Cell).then_some(why)
        };
        match reporter.report(&self.server, Direction::ToServer, msg) {
            Ok(Response::Decision {
                decision: Decision::Deny | Decision::Ask,
                reason,
            }) if is_call => Some(reason.unwrap_or_else(|| "denied".into())),
            Ok(Response::Error { message }) => {
                fail(format!("wyrdline rejected the message: {message}"))
            }
            Ok(_) => None,
            Err(e) => fail(format!("wyrdline unavailable: {e}")),
        }
    }

    /// Handle one line from the agent.
    pub fn from_client(&self, line: &str) -> FromClient {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // Not JSON: the server will answer with a parse error.
            return FromClient::Forward(line.to_string());
        };
        match v {
            Value::Array(items) => {
                let mut forward = Vec::new();
                let mut reply = Vec::new();
                for item in items {
                    if let Some(reason) = self.check(&item) {
                        let id = item.get("id").unwrap_or(&Value::Null);
                        reply.push(denial(id, &reason));
                    } else {
                        forward.push(item);
                    }
                }
                match (forward.is_empty(), reply.is_empty()) {
                    (_, true) => FromClient::Forward(Value::Array(forward).to_string()),
                    (true, false) => FromClient::Reply(Value::Array(reply).to_string()),
                    (false, false) => FromClient::Both {
                        forward: Value::Array(forward).to_string(),
                        reply: Value::Array(reply).to_string(),
                    },
                }
            }
            msg => match self.check(&msg) {
                Some(reason) => FromClient::Reply(
                    denial(msg.get("id").unwrap_or(&Value::Null), &reason).to_string(),
                ),
                None => FromClient::Forward(line.to_string()),
            },
        }
    }

    /// Record one line from the server (it is always relayed unchanged).
    pub fn from_server(&self, line: &str) {
        let Some(reporter) = &self.reporter else {
            return;
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let items = match v {
            Value::Array(items) => items,
            one => vec![one],
        };
        for item in items {
            if let Err(e) = reporter.report(&self.server, Direction::ToClient, &item) {
                tracing::warn!(server = %self.server, error = %e, "could not record an MCP response");
            }
        }
    }
}

pub(crate) fn write_line(out: &Mutex<Box<dyn Write + Send>>, line: &str) -> std::io::Result<()> {
    let mut w = out.lock().unwrap_or_else(PoisonError::into_inner);
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Run the stdio proxy until the server exits; returns its exit code.
pub fn run_stdio<R: Reporter + 'static>(
    gateway: Gateway<R>,
    command: &[String],
    input: impl Read + Send + 'static,
    output: Box<dyn Write + Send>,
) -> std::io::Result<i32> {
    let (prog, args) = command.split_first().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no MCP server command")
    })?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut to_server = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("no stdin"))?;
    let from_server = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("no stdout"))?;
    let gateway = Arc::new(gateway);
    let out = Arc::new(Mutex::new(output));

    let (g, o) = (Arc::clone(&gateway), Arc::clone(&out));
    let upstream = std::thread::spawn(move || -> std::io::Result<()> {
        for line in BufReader::new(input).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let (forward, reply) = match g.from_client(&line) {
                FromClient::Forward(f) => (Some(f), None),
                FromClient::Reply(r) => (None, Some(r)),
                FromClient::Both { forward, reply } => (Some(forward), Some(reply)),
            };
            if let Some(r) = reply {
                write_line(&o, &r)?;
            }
            if let Some(f) = forward {
                to_server.write_all(f.as_bytes())?;
                to_server.write_all(b"\n")?;
                to_server.flush()?;
            }
        }
        // Agent closed its side: close the server's stdin so it can exit.
        Ok(())
    });

    for line in BufReader::new(from_server).lines() {
        let line = line?;
        gateway.from_server(&line);
        write_line(&out, &line)?;
    }
    let status = child.wait()?;
    // The upstream thread ends when the agent closes stdin; a server that
    // exits first leaves it blocked on read, which ends with the process.
    if upstream.is_finished() {
        let _ = upstream.join();
    }
    Ok(status.code().unwrap_or(1))
}
