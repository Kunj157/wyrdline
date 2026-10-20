//! Request and response bodies of the daemon API. Every type here is part
//! of the published contract: it derives `JsonSchema` (the `OpenAPI` document)
//! and `TS` (generated Studio types).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use wyrd_cell::apply::ApplyReport;
use wyrd_cell::diff::Change;
use wyrd_cell::state::{Branch, CheckpointRecord, SessionRecord};
use wyrd_correlator::{Call, Effect};
use wyrd_ledger::Verification;
use wyrd_schema::Event;

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Health {
    pub status: String,
    pub version: String,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Ready {
    pub status: String,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct SessionList {
    /// Newest first.
    pub sessions: Vec<SessionRecord>,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct SessionDetail {
    #[serde(flatten)]
    #[ts(flatten)]
    pub record: SessionRecord,
    pub head_seq: u64,
    /// `b3:` + hex of the last event's hash.
    pub head_hash: String,
    pub sealed: bool,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct EventPage {
    pub events: Vec<Event>,
    /// Pass as `after_seq` for the next page; null on the last page.
    pub next_after_seq: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct ToolCallDetail {
    pub call: Call,
    /// Effects attributed to the call or naming it as an ambiguous candidate.
    pub effects: Vec<Effect>,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct VerifyReport {
    #[serde(flatten)]
    #[ts(flatten)]
    pub verification: Verification,
    pub ok: bool,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Checkpoints {
    pub current_branch: String,
    pub branches: Vec<Branch>,
    pub checkpoints: Vec<CheckpointRecord>,
}

/// An uploaded policy component.
#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct PolicyInfo {
    pub id: String,
    /// `b3:` + 16 hex digits of the component's BLAKE3 hash.
    pub version: String,
    #[ts(type = "number")]
    pub bytes: u64,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct PolicyList {
    pub policies: Vec<PolicyInfo>,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct BacktestBody {
    pub session_id: String,
}

/// One action the policy would have stopped.
#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct BacktestFinding {
    #[ts(type = "number")]
    pub seq: u64,
    /// `hook`, `gateway`, `kernel`, or `effect`.
    pub stage: String,
    /// `deny` or `ask`.
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct BacktestReport {
    pub policy_id: String,
    pub policy_version: String,
    pub session_id: String,
    #[ts(type = "number")]
    pub events: u64,
    pub findings: Vec<BacktestFinding>,
    pub notes: Vec<String>,
}

/// A session's divergences with their triage state, in ledger order.
#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct DivergenceList {
    pub divergences: Vec<wyrd_divergence::view::DivergenceView>,
}

/// Triage of one divergence.
#[derive(Debug, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TriageBody {
    pub status: wyrd_schema::DivergenceStatus,
    /// Why; stored in the ledger (keep it free of secrets). At most 2000 characters.
    #[serde(default)]
    pub note: Option<String>,
}

/// Questions a running session's policy is waiting on.
#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct PendingAsks {
    pub asks: Vec<wyrd_adapters::asks::PendingAsk>,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct AnswerBody {
    pub allow: bool,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Answered {
    #[ts(type = "number")]
    pub id: u64,
    pub allow: bool,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Stopped {
    /// False when no agent was running.
    pub stopped: bool,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct BranchCreated {
    pub branch: Branch,
}

/// A rewind or fork handed to a detached `wyrd … --relaunch` (202).
#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Relaunching {
    pub relaunching: bool,
    pub pid: u32,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct Changes {
    pub changes: Vec<Change>,
}

/// One side of a file diff.
#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct FileSide {
    pub source: wyrd_cell::session::FileSource,
    /// UTF-8 text; null when absent or binary.
    pub text: Option<String>,
    pub binary: bool,
    /// Longer than the limit; `text` holds the beginning.
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema, TS)]
pub struct FileDiff {
    pub path: String,
    pub before: FileSide,
    pub after: FileSide,
}

/// The apply report (re-exported for the contract).
pub type ApplyResult = ApplyReport;

/// The OTLP `ExportTraceServiceResponse` (always a full success).
#[derive(Debug, Serialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct OtlpExportResponse {
    pub partial_success: PartialSuccess,
}

#[derive(Debug, Default, Serialize, JsonSchema, TS)]
pub struct PartialSuccess {}

#[derive(Debug, Default, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct RewindBody {
    /// Start the agent again on the new branch (handled by a detached `wyrd`).
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub relaunch: bool,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ForkBody {
    /// Name of the new branch.
    pub name: String,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub relaunch: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ApplyBody {
    /// List the operations without changing anything.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub dry_run: bool,
    /// Apply even if the real workspace changed since the session started.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub force: bool,
}
