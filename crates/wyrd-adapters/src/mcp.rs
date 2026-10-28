//! MCP traffic seen by the gateway → intent events.
//!
//! Tracks requests in flight per server to pair responses with their
//! method, maps `tools/call` to tool-call events, and pins every tool's
//! description at its first `tools/list` so a later change (a "rug pull":
//! a server swapping a benign description for a malicious one after it
//! was approved) is flagged.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wyrd_bundle::redact::{redact_json, redact_str};
use wyrd_schema::{CallStatus, EventBody, McpRequest, McpResponse, ToolCallFinished};

use crate::ingest::INGEST_LEVEL;
use crate::protocol::Direction;

/// Maximum characters kept from a result summary.
const SUMMARY_CHARS: usize = 400;

/// Policy id of tool-description pinning decisions.
pub const PINNING_POLICY: &str = "wyrdline.mcp-pinning";

/// File, in a session directory, that keeps the session's tool pins across
/// restarts of its gateway.
pub const PINS_FILE: &str = "mcp-pins.json";

const PINS_VERSION: u32 = 1;

/// Why the pins could not be loaded or saved. Each one stops the session's
/// MCP traffic: a pin that is not on disk cannot be compared after a restart.
#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("cannot read the MCP tool pins at {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "the MCP tool pins at {} are corrupt ({reason}); refusing to continue. \
         Remove the file only if you trust the MCP servers to be unchanged",
        path.display()
    )]
    Corrupt { path: PathBuf, reason: String },
    #[error(
        "the MCP tool pins at {} are readable by other users (mode {mode:o}); \
         refusing to load them",
        path.display()
    )]
    Unsafe { path: PathBuf, mode: u32 },
    #[error("cannot save the MCP tool pins to {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "an earlier failure to save the MCP tool pins makes them unsafe to use; restart the session"
    )]
    Broken,
}

/// On-disk form of the pins: `{"version": 1, "pins": [...]}`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinFile {
    version: u32,
    pins: Vec<PinEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinEntry {
    server: String,
    tool: String,
    /// BLAKE3 of the tool's description and schema, hex.
    blake3: String,
}

type Pins = HashMap<(String, String), blake3::Hash>;

/// Read the pins at `path`. A missing file means no pins yet. Anything else
/// that is not a well-formed, private file fails closed.
fn load(path: &Path) -> Result<Pins, PinError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pins::new()),
        Err(source) => {
            return Err(PinError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let corrupt = |reason: String| PinError::Corrupt {
        path: path.to_path_buf(),
        reason,
    };
    if !meta.is_file() {
        return Err(corrupt("not a regular file".into()));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(PinError::Unsafe {
            path: path.to_path_buf(),
            mode: meta.mode() & 0o777,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|source| PinError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let file: PinFile = serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
    if file.version != PINS_VERSION {
        return Err(corrupt(format!("unknown version {}", file.version)));
    }
    let mut pins = Pins::new();
    for entry in file.pins {
        let hash = blake3::Hash::from_hex(&entry.blake3)
            .map_err(|e| corrupt(format!("{}/{}: bad hash: {e}", entry.server, entry.tool)))?;
        let key = (entry.server, entry.tool);
        if pins.insert(key.clone(), hash).is_some() {
            return Err(corrupt(format!("{}/{} is pinned twice", key.0, key.1)));
        }
    }
    Ok(pins)
}

/// Replace the pins at `path` atomically: a private temporary file, synced,
/// then renamed over the old one. A crash leaves either the old or the new
/// file, never a partial one.
fn save(path: &Path, pins: &Pins) -> Result<(), PinError> {
    let mut entries: Vec<PinEntry> = pins
        .iter()
        .map(|((server, tool), hash)| PinEntry {
            server: server.clone(),
            tool: tool.clone(),
            blake3: hash.to_hex().to_string(),
        })
        .collect();
    entries.sort_by(|a, b| (&a.server, &a.tool).cmp(&(&b.server, &b.tool)));
    let text = serde_json::to_string_pretty(&PinFile {
        version: PINS_VERSION,
        pins: entries,
    })
    .map_err(|e| PinError::Write {
        path: path.to_path_buf(),
        source: std::io::Error::other(e),
    })?;
    let tmp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        // A leftover temporary file keeps its old mode; start from nothing.
        let _ = std::fs::remove_file(&tmp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent().and_then(|d| std::fs::File::open(d).ok()) {
            let _ = dir.sync_all();
        }
        Ok(())
    };
    write().map_err(|source| PinError::Write {
        path: path.to_path_buf(),
        source,
    })
}

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
    pins: Pins,
    /// Where pins persist, if anywhere. Without it they live for one run.
    store: Option<PathBuf>,
    /// Set after a failed save: the pins on disk no longer match memory.
    broken: bool,
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
    /// State whose pins persist at `path` (a session's [`PINS_FILE`]), so a
    /// restarted gateway compares tools against the first listing ever seen.
    /// A corrupt or unsafe file is an error, never an empty pin set.
    pub fn with_store(path: PathBuf) -> Result<Self, PinError> {
        let pins = load(&path)?;
        Ok(McpState {
            pins,
            store: Some(path),
            ..McpState::default()
        })
    }

    /// Interpret one message. Batches (arrays) are observed element-wise
    /// by the caller.
    pub fn observe(
        &mut self,
        server: &str,
        direction: Direction,
        msg: &Value,
        now: Instant,
    ) -> Result<Observed, PinError> {
        if self.broken {
            return Err(PinError::Broken);
        }
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
                    return Ok(out);
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
                    out.rug_pulls = self.pin(server, result)?;
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
        Ok(out)
    }

    /// Record each tool's description; return tools whose description
    /// differs from the first one seen. A first sighting is saved before
    /// the listing is answered.
    fn pin(&mut self, server: &str, result: &Value) -> Result<Vec<String>, PinError> {
        let mut changed = Vec::new();
        let mut new_pin = false;
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
                    new_pin = true;
                }
                std::collections::hash_map::Entry::Occupied(o) => {
                    if *o.get() != h {
                        changed.push(name.to_string());
                    }
                }
            }
        }
        if new_pin {
            self.persist()?;
        }
        Ok(changed)
    }

    /// Save the pins. After a failure the pins on disk are stale, so every
    /// later message is refused.
    fn persist(&mut self) -> Result<(), PinError> {
        let Some(path) = &self.store else {
            return Ok(());
        };
        if let Err(e) = save(path, &self.pins) {
            self.broken = true;
            return Err(e);
        }
        Ok(())
    }
}
