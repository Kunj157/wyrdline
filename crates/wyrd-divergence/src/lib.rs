//! Intent–effect divergence: what an agent claimed versus what the ledger
//! shows (plan §17.4–17.5, ADR-006).
//!
//! [`evaluate`] is a pure function: the session's events, its correlator
//! graph, the extracted claims, the before/after contents of changed test
//! files, and a [`Config`] in; divergences out, each citing the ledger
//! events that prove it. Claims come from [`claims::extract_all`] (regex)
//! and optionally an LLM extractor; either way a claim whose quote is not
//! verbatim in its turn is dropped before any rule sees it.

pub mod case;
pub mod claims;
pub mod config;
pub mod glob;
pub mod llm;
mod rules;
mod session;
pub mod view;

use std::collections::BTreeMap;

pub use config::{Config, ConfigError};
pub use rules::{PINNING_POLICY, test_counts};
use wyrd_correlator::Graph;
use wyrd_schema::{ClaimExtracted, DivergenceRaised, Event, Severity};

/// Rule ids with their names, in order.
pub const RULES: [(&str, &str); 10] = [
    ("D001", "claim-without-evidence"),
    ("D002", "out-of-scope-write"),
    ("D003", "secret-read-then-egress"),
    ("D004", "unattributed-effect"),
    ("D005", "silent-failure"),
    ("D006", "test-tampering"),
    ("D007", "hidden-network"),
    ("D008", "mass-destruction"),
    ("D009", "tool-rug-pull"),
    ("D010", "claim-scope-mismatch"),
];

/// Name of a rule id (`D003` → `secret-read-then-egress`).
pub fn rule_name(id: &str) -> Option<&'static str> {
    RULES.iter().find(|(r, _)| *r == id).map(|(_, n)| *n)
}

/// A changed file's contents before the session and at its end (`None`:
/// did not exist / was deleted). Only test files are needed (D006).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileChange {
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Everything the rules look at.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub events: &'a [Event],
    pub graph: &'a Graph,
    pub claims: &'a [ClaimExtracted],
    /// Workspace-relative path → contents.
    pub files: &'a BTreeMap<String, FileChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    rule_id: &'static str,
    severity: Severity,
    claim_id: Option<String>,
    evidence_seqs: Vec<u64>,
    confidence_bp: u16,
    summary: String,
}

/// Deterministic id: the same rule, claim, and evidence always get the
/// same id, so re-evaluating a session does not duplicate divergences.
pub fn divergence_id(rule: &str, claim_id: Option<&str>, evidence: &[u64]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(rule.as_bytes());
    h.update(b"\0");
    h.update(claim_id.unwrap_or_default().as_bytes());
    for s in evidence {
        h.update(&s.to_le_bytes());
    }
    format!("dv-{}", &hex::encode(h.finalize().as_bytes())[..12])
}

/// Run every rule. Output is sorted by rule, then first evidence seq.
pub fn evaluate(input: &Input<'_>, config: &Config) -> Vec<DivergenceRaised> {
    let session = session::Session::new(input.events, input.graph);
    let turns = claims::assistant_turns(input.events);
    let verified = claims::merge(&turns, input.claims.iter().cloned());
    let input = Input {
        claims: &verified,
        ..*input
    };
    let mut out: Vec<DivergenceRaised> = rules::all(&input, &session, config, &turns)
        .into_iter()
        .map(|f| DivergenceRaised {
            divergence_id: divergence_id(f.rule_id, f.claim_id.as_deref(), &f.evidence_seqs),
            rule_id: f.rule_id.to_string(),
            severity: f.severity,
            claim_id: f.claim_id,
            evidence_seqs: f.evidence_seqs,
            confidence_bp: f.confidence_bp,
            summary: f.summary,
        })
        .collect();
    out.sort_by(|a, b| {
        (a.rule_id.as_str(), a.evidence_seqs.first())
            .cmp(&(b.rule_id.as_str(), b.evidence_seqs.first()))
    });
    out.dedup_by(|a, b| a.divergence_id == b.divergence_id);
    out
}

/// Regex claims plus every rule, for a session without other extractors.
pub fn evaluate_rules_only(
    events: &[Event],
    graph: &Graph,
    files: &BTreeMap<String, FileChange>,
    config: &Config,
) -> (Vec<ClaimExtracted>, Vec<DivergenceRaised>) {
    let claims = claims::extract_all(&claims::assistant_turns(events));
    let divergences = evaluate(
        &Input {
            events,
            graph,
            claims: &claims,
            files,
        },
        config,
    );
    (claims, divergences)
}
