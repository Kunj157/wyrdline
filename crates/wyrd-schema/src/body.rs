//! Kind-specific event payloads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::Plane;

/// Every event kind of schema v1, adjacently tagged as
/// `{ "kind": "effect.file.write", "payload": { … } }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", content = "payload")]
pub enum EventBody {
    // ---- meta ----
    #[serde(rename = "meta.session.started")]
    SessionStarted(SessionStarted),
    #[serde(rename = "meta.session.closed")]
    SessionClosed(SessionClosed),
    #[serde(rename = "meta.session.state_changed")]
    SessionStateChanged(SessionStateChanged),
    #[serde(rename = "meta.sensor.lost")]
    SensorLost(SensorLost),
    #[serde(rename = "meta.sensor.resumed")]
    SensorResumed(SensorResumed),

    // ---- intent ----
    #[serde(rename = "intent.tool_call.started")]
    ToolCallStarted(ToolCallStarted),
    #[serde(rename = "intent.tool_call.finished")]
    ToolCallFinished(ToolCallFinished),
    #[serde(rename = "intent.tool_call.denied")]
    ToolCallDenied(ToolCallDenied),
    #[serde(rename = "intent.message")]
    Message(Message),
    #[serde(rename = "intent.mcp.request")]
    McpRequest(McpRequest),
    #[serde(rename = "intent.mcp.response")]
    McpResponse(McpResponse),

    // ---- effect ----
    #[serde(rename = "effect.proc.exec")]
    ProcExec(ProcExec),
    #[serde(rename = "effect.proc.fork")]
    ProcFork(ProcFork),
    #[serde(rename = "effect.proc.exit")]
    ProcExit(ProcExit),
    #[serde(rename = "effect.file.open_write")]
    FileOpenWrite(FileOpenWrite),
    #[serde(rename = "effect.file.write")]
    FileWrite(FileWrite),
    #[serde(rename = "effect.file.rename")]
    FileRename(FileRename),
    #[serde(rename = "effect.file.unlink")]
    FileUnlink(FileUnlink),
    #[serde(rename = "effect.file.mkdir")]
    FileMkdir(FilePath),
    #[serde(rename = "effect.file.chmod")]
    FileChmod(FileChmod),
    #[serde(rename = "effect.file.truncate")]
    FileTruncate(FileTruncate),
    #[serde(rename = "effect.file.read_sensitive")]
    FileReadSensitive(FilePath),
    #[serde(rename = "effect.net.connect")]
    NetConnect(NetConnect),
    #[serde(rename = "effect.net.dns_query")]
    DnsQuery(DnsQuery),

    // ---- state ----
    #[serde(rename = "state.checkpoint.created")]
    CheckpointCreated(CheckpointCreated),
    #[serde(rename = "state.checkpoint.rewound")]
    CheckpointRewound(CheckpointRewound),
    #[serde(rename = "state.checkpoint.forked")]
    CheckpointForked(CheckpointForked),
    #[serde(rename = "state.checkpoint.applied")]
    CheckpointApplied(CheckpointApplied),

    // ---- guard ----
    #[serde(rename = "guard.decision")]
    GuardDecision(GuardDecision),

    // ---- verdict ----
    #[serde(rename = "verdict.claim.extracted")]
    ClaimExtracted(ClaimExtracted),
    #[serde(rename = "verdict.divergence.raised")]
    DivergenceRaised(DivergenceRaised),
    #[serde(rename = "verdict.divergence.updated")]
    DivergenceUpdated(DivergenceUpdated),
}

macro_rules! kinds {
    ($($variant:ident => $plane:ident, $kind:literal;)*) => {
        impl EventBody {
            /// Plane of this kind.
            pub fn plane(&self) -> Plane {
                match self { $(EventBody::$variant(_) => Plane::$plane,)* }
            }

            /// Dotted kind name, identical to the serialised tag.
            pub fn kind(&self) -> &'static str {
                match self { $(EventBody::$variant(_) => $kind,)* }
            }
        }

        /// Every kind name of schema v1 with its plane.
        pub const KINDS: &[(&str, Plane)] = &[$(($kind, Plane::$plane),)*];
    };
}

kinds! {
    SessionStarted => Meta, "meta.session.started";
    SessionClosed => Meta, "meta.session.closed";
    SessionStateChanged => Meta, "meta.session.state_changed";
    SensorLost => Meta, "meta.sensor.lost";
    SensorResumed => Meta, "meta.sensor.resumed";
    ToolCallStarted => Intent, "intent.tool_call.started";
    ToolCallFinished => Intent, "intent.tool_call.finished";
    ToolCallDenied => Intent, "intent.tool_call.denied";
    Message => Intent, "intent.message";
    McpRequest => Intent, "intent.mcp.request";
    McpResponse => Intent, "intent.mcp.response";
    ProcExec => Effect, "effect.proc.exec";
    ProcFork => Effect, "effect.proc.fork";
    ProcExit => Effect, "effect.proc.exit";
    FileOpenWrite => Effect, "effect.file.open_write";
    FileWrite => Effect, "effect.file.write";
    FileRename => Effect, "effect.file.rename";
    FileUnlink => Effect, "effect.file.unlink";
    FileMkdir => Effect, "effect.file.mkdir";
    FileChmod => Effect, "effect.file.chmod";
    FileTruncate => Effect, "effect.file.truncate";
    FileReadSensitive => Effect, "effect.file.read_sensitive";
    NetConnect => Effect, "effect.net.connect";
    DnsQuery => Effect, "effect.net.dns_query";
    CheckpointCreated => State, "state.checkpoint.created";
    CheckpointRewound => State, "state.checkpoint.rewound";
    CheckpointForked => State, "state.checkpoint.forked";
    CheckpointApplied => State, "state.checkpoint.applied";
    GuardDecision => Guard, "guard.decision";
    ClaimExtracted => Verdict, "verdict.claim.extracted";
    DivergenceRaised => Verdict, "verdict.divergence.raised";
    DivergenceUpdated => Verdict, "verdict.divergence.updated";
}

// ---------------------------------------------------------------- meta

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Copy-on-write sandbox with checkpoints and rewind.
    Cell,
    /// Observation only, no sandbox and no rewind.
    Observe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Preparing,
    Recording,
    Paused,
    Rewinding,
    Degraded,
    Closing,
    Closed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SessionStarted {
    pub session_id: String,
    /// Adapter name, e.g. `claude-code`, `codex`, `gemini-cli`, `mcp`, `fakeagent`.
    pub agent_kind: String,
    pub agent_version: Option<String>,
    /// Redacted agent command line.
    pub command: Vec<String>,
    pub workspace_root: String,
    pub mode: CaptureMode,
    pub kernel_release: String,
    /// Capability name → available, as reported by `wyrd doctor`.
    pub capabilities: std::collections::BTreeMap<String, bool>,
    pub policy_set_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SessionClosed {
    pub reason: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SessionStateChanged {
    pub from: SessionState,
    pub to: SessionState,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SensorLost {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SensorResumed {
    /// Events known to be lost while the sensor was unavailable.
    #[ts(type = "number")]
    pub dropped: u64,
}

// ---------------------------------------------------------------- intent

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum IntentSource {
    Hook,
    Mcp,
    Otlp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Ok,
    Error,
    Denied,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCallStarted {
    pub call_id: String,
    pub source: IntentSource,
    pub tool: String,
    /// Redacted tool arguments.
    pub args: serde_json::Value,
    /// Transcript turn that issued the call, when known.
    pub turn: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCallFinished {
    pub call_id: String,
    pub status: CallStatus,
    pub exit_code: Option<i32>,
    /// Short redacted summary of the result.
    pub summary: Option<String>,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCallDenied {
    pub call_id: String,
    pub policy_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub turn: u32,
    /// Redacted message text.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct McpRequest {
    pub server: String,
    pub request_id: String,
    pub method: String,
    /// Redacted params.
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct McpResponse {
    pub server: String,
    pub request_id: String,
    pub is_error: bool,
    pub result_summary: Option<String>,
}

// ---------------------------------------------------------------- effect

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ProcExec {
    pub filename: String,
    /// First arguments, each possibly truncated (see `truncated`).
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ProcFork {
    pub child_pid: u32,
    #[ts(type = "number")]
    pub child_start_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ProcExit {
    pub exit_code: i32,
    /// Terminating signal, if the process was killed.
    pub signal: Option<i32>,
}

/// A path as observed in the kernel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FilePath {
    pub path: String,
    /// True when the path is inside the session's Cell (rewindable).
    pub in_cell: bool,
    /// True when path resolution hit its component limit.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileOpenWrite {
    pub file: FilePath,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileWrite {
    pub file: FilePath,
    /// Bytes written, aggregated per file descriptor per window.
    #[ts(type = "number")]
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileRename {
    pub from: FilePath,
    pub to: FilePath,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileUnlink {
    pub file: FilePath,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileChmod {
    pub file: FilePath,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileTruncate {
    pub file: FilePath,
    #[ts(type = "number")]
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum NetVerdict {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct NetConnect {
    /// `4` or `6`.
    pub family: u8,
    pub addr: String,
    pub port: u16,
    /// Most recent DNS name that resolved to `addr` in this session.
    pub dns_name: Option<String>,
    pub verdict: NetVerdict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct DnsQuery {
    pub name: String,
}

// ---------------------------------------------------------------- state

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CheckpointCreated {
    pub checkpoint_id: String,
    pub branch_id: String,
    pub parent_id: Option<String>,
    pub after_call_id: Option<String>,
    /// BLAKE3 of the manifest, `b3:` + hex.
    pub manifest: String,
    pub files_changed: u32,
    #[ts(type = "number")]
    pub bytes_added: u64,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRewound {
    pub checkpoint_id: String,
    pub branch_id: String,
    /// Effects that could not be undone (escaped the Cell or hit the network).
    #[ts(type = "number[]")]
    pub irreversible_seqs: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CheckpointForked {
    pub checkpoint_id: String,
    pub branch_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CheckpointApplied {
    pub checkpoint_id: String,
    pub files: u32,
}

// ---------------------------------------------------------------- guard

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum GuardStage {
    Hook,
    Gateway,
    Kernel,
    Effect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GuardDecision {
    pub stage: GuardStage,
    pub decision: Decision,
    pub policy_id: String,
    pub policy_version: String,
    pub reason: String,
    pub call_id: Option<String>,
    /// Effect event the decision refers to, when it is effect-time.
    #[ts(type = "number | null")]
    pub subject_seq: Option<u64>,
}

// ---------------------------------------------------------------- verdict

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    TestsPass,
    BuildOk,
    RanCommand,
    ChangedOnly,
    DidNotChange,
    NoNetwork,
    FixedIssue,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum Extractor {
    Rule,
    Llm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ClaimExtracted {
    pub claim_id: String,
    pub turn: u32,
    pub claim_type: ClaimType,
    pub subject: String,
    /// Verbatim substring of the referenced assistant turn.
    pub quote: String,
    pub extractor: Extractor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceStatus {
    Open,
    Acknowledged,
    FalsePositive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct DivergenceRaised {
    pub divergence_id: String,
    /// Rule identifier, e.g. `D001`.
    pub rule_id: String,
    pub severity: Severity,
    pub claim_id: Option<String>,
    /// Ledger events that prove the divergence.
    #[ts(type = "number[]")]
    pub evidence_seqs: Vec<u64>,
    /// Confidence in basis points (0–10000), integer for deterministic hashing.
    pub confidence_bp: u16,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct DivergenceUpdated {
    pub divergence_id: String,
    pub status: DivergenceStatus,
    pub note: Option<String>,
}
