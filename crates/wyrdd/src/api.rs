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
    pub relaunch: bool,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ForkBody {
    /// Name of the new branch.
    pub name: String,
    #[serde(default)]
    pub relaunch: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ApplyBody {
    /// List the operations without changing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Apply even if the real workspace changed since the session started.
    #[serde(default)]
    pub force: bool,
}
