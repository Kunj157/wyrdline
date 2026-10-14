//! Landlock write confinement for the agent process.

use std::path::{Path, PathBuf};

use landlock::{
    ABI, AccessFs, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    path_beneath_rules,
};
use serde::{Deserialize, Serialize};

use crate::{CellError, Result};

/// Highest Landlock ABI whose filesystem rights we request.
const TARGET_ABI: ABI = ABI::V5;

/// How completely the ruleset is enforced by the running kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    Full,
    Partial,
    None,
}

/// Default writable locations besides the workspace: temporary files,
/// device nodes such as `/dev/null`, and the dot-directories coding agents
/// keep their own state in. Writes there escape the Cell and are recorded
/// as irreversible.
pub fn default_write_allow(workspace: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = vec![
        workspace.to_path_buf(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/dev"),
        PathBuf::from("/proc/self"),
    ];
    if let Some(h) = home {
        for d in [
            ".cache",
            ".claude",
            ".claude.json",
            ".codex",
            ".gemini",
            ".config",
            ".npm",
        ] {
            v.push(h.join(d));
        }
    }
    v
}

/// Restrict the calling thread (and every later child) so that it can only
/// modify paths beneath `allow`. Reading and executing stay unrestricted.
/// Missing paths are ignored.
pub fn restrict_writes(allow: &[PathBuf]) -> Result<Enforcement> {
    let write = AccessFs::from_write(TARGET_ABI);
    let existing: Vec<&PathBuf> = allow.iter().filter(|p| p.exists()).collect();
    let status: RestrictionStatus = Ruleset::default()
        .handle_access(write)
        .map_err(|e| CellError::Sys(format!("landlock: {e}")))?
        .create()
        .map_err(|e| CellError::Sys(format!("landlock: {e}")))?
        .add_rules(path_beneath_rules(existing, write))
        .map_err(|e| CellError::Sys(format!("landlock: {e}")))?
        .restrict_self()
        .map_err(|e| CellError::Sys(format!("landlock: {e}")))?;
    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced => Enforcement::Full,
        RulesetStatus::PartiallyEnforced => Enforcement::Partial,
        RulesetStatus::NotEnforced => Enforcement::None,
    })
}
