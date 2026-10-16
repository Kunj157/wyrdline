//! Wire protocol of the intent socket: one JSON request line, one JSON
//! response line per connection.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wyrd_schema::Decision;

/// Upper bound on one request or response line.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// A hook payload from an agent adapter. A payload that was not valid
    /// JSON arrives as a string and is recorded as malformed.
    Hook {
        adapter: String,
        payload: serde_json::Value,
    },
    /// A JSON-RPC message seen by the MCP gateway for `server`.
    Mcp {
        server: String,
        direction: Direction,
        message: serde_json::Value,
    },
    /// An OTLP/HTTP JSON trace export (forwarded by the daemon); its
    /// generative-AI tool spans become intent events.
    Otlp {
        request: serde_json::Value,
    },
    Ping,
}

/// Which way an MCP message travelled through the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Agent (client) → server.
    ToServer,
    /// Server → agent (client).
    ToClient,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Decision for a pre-execution check.
    Decision {
        decision: Decision,
        reason: Option<String>,
    },
    Ok,
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("intent socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("intent socket: bad message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("intent socket closed without a response")]
    NoResponse,
    #[error("intent socket: message longer than {MAX_LINE} bytes")]
    TooLong,
}

/// Read one newline-terminated line of at most [`MAX_LINE`] bytes.
pub fn read_line(stream: impl std::io::Read) -> Result<Option<String>, ProtocolError> {
    let mut reader = BufReader::new(stream.take(MAX_LINE as u64 + 1));
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    if line.len() > MAX_LINE {
        return Err(ProtocolError::TooLong);
    }
    Ok(Some(line))
}

/// Send one request and wait for the response.
pub fn call(socket: &Path, req: &Request, timeout: Duration) -> Result<Response, ProtocolError> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(req)?;
    if line.len() >= MAX_LINE {
        return Err(ProtocolError::TooLong);
    }
    line.push(b'\n');
    stream.write_all(&line)?;
    let resp = read_line(&stream)?.ok_or(ProtocolError::NoResponse)?;
    Ok(serde_json::from_str(&resp)?)
}
