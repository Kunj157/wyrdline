//! Intent capture.
//!
//! Agents run inside the Cell and cannot write the ledger, so their hooks
//! and the MCP gateway (every proxied JSON-RPC message) talk to a
//! per-session **intent socket** served by `wyrd run` outside the Cell.
//! The socket accepts ingest messages, read-only session views, and
//! rewind requests that wait for a human — never commands — and
//! everything it receives is untrusted: kernel effects (Phase 5) are the
//! ground truth intent is checked against.
#![forbid(unsafe_code)]

pub mod asks;
pub mod claude_code;
pub mod daemon;
pub mod gemini_cli;
pub mod hook;
pub mod ingest;
pub mod mcp;
pub mod protocol;
pub mod server;

pub use ingest::{HookEvent, Ingest, SessionViews, Sink, StaticPolicy, ToolPolicy};
pub use protocol::{Direction, Request, Response, SessionQuery};

/// Environment variable carrying the intent socket path into the Cell.
pub const SOCKET_ENV: &str = "WYRD_INTENT_SOCKET";
/// Environment variable carrying the capture mode (`cell` or `observe`).
pub const MODE_ENV: &str = "WYRD_MODE";
/// Environment variable carrying the path of the `wyrd` binary.
pub const BIN_ENV: &str = "WYRD_BIN";

/// Adapter name of the Codex CLI. Its hooks use the same payload and
/// decision format as the claude-code adapter.
pub const CODEX: &str = "codex";

/// Hook adapters known to the intent socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    ClaudeCode,
    Codex,
    GeminiCli,
}

impl Adapter {
    pub fn from_name(name: &str) -> Option<Adapter> {
        match name {
            claude_code::NAME => Some(Adapter::ClaudeCode),
            CODEX => Some(Adapter::Codex),
            gemini_cli::NAME => Some(Adapter::GeminiCli),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Adapter::ClaudeCode => claude_code::NAME,
            Adapter::Codex => CODEX,
            Adapter::GeminiCli => gemini_cli::NAME,
        }
    }

    /// Parse a hook payload into an adapter-neutral event.
    pub fn parse(self, payload: &serde_json::Value) -> Result<HookEvent, String> {
        match self {
            Adapter::ClaudeCode | Adapter::Codex => claude_code::parse(payload),
            Adapter::GeminiCli => gemini_cli::parse(payload),
        }
    }

    /// Hook stdout for a pre-execution decision; `None` defers to the
    /// agent's own permission flow.
    pub fn decision_output(
        self,
        decision: wyrd_schema::Decision,
        reason: Option<&str>,
    ) -> Option<serde_json::Value> {
        match self {
            Adapter::ClaudeCode | Adapter::Codex => claude_code::decision_output(decision, reason),
            Adapter::GeminiCli => gemini_cli::decision_output(decision, reason),
        }
    }

    /// Whether a raw payload is a pre-execution hook (must be answered).
    pub fn is_pre_tool(self, payload: &serde_json::Value) -> bool {
        match self {
            Adapter::ClaudeCode | Adapter::Codex => {
                claude_code::event_name(payload) == "PreToolUse"
            }
            Adapter::GeminiCli => gemini_cli::event_name(payload) == "BeforeTool",
        }
    }
}
