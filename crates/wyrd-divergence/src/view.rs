//! Divergences as recorded in a ledger, with their current triage state.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use ts_rs::TS;
use wyrd_schema::{ClaimExtracted, DivergenceRaised, DivergenceStatus, Event, EventBody};

/// One recorded divergence, folded with its triage updates.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema, TS)]
pub struct DivergenceView {
    /// Seq of the `verdict.divergence.raised` event.
    #[ts(type = "number")]
    pub seq: u64,
    #[serde(flatten)]
    #[ts(flatten)]
    pub divergence: DivergenceRaised,
    /// Rule name, e.g. `secret-read-then-egress`.
    pub rule_name: String,
    pub status: DivergenceStatus,
    pub note: Option<String>,
    /// The claim the divergence is about, when there is one.
    pub claim: Option<ClaimExtracted>,
}

/// Every divergence raised in `events`, in ledger order, with the latest
/// triage state (later updates win; a duplicate raise is ignored).
pub fn project(events: &[Event]) -> Vec<DivergenceView> {
    let mut claims: BTreeMap<&str, &ClaimExtracted> = BTreeMap::new();
    let mut out: Vec<DivergenceView> = Vec::new();
    for e in events {
        match &e.body {
            EventBody::ClaimExtracted(c) => {
                claims.insert(&c.claim_id, c);
            }
            EventBody::DivergenceRaised(d) => {
                if out
                    .iter()
                    .any(|v| v.divergence.divergence_id == d.divergence_id)
                {
                    continue;
                }
                out.push(DivergenceView {
                    seq: e.seq,
                    divergence: d.clone(),
                    rule_name: crate::rule_name(&d.rule_id).unwrap_or("unknown").into(),
                    status: DivergenceStatus::Open,
                    note: None,
                    claim: d
                        .claim_id
                        .as_deref()
                        .and_then(|id| claims.get(id).map(|c| (*c).clone())),
                });
            }
            EventBody::DivergenceUpdated(u) => {
                if let Some(v) = out
                    .iter_mut()
                    .find(|v| v.divergence.divergence_id == u.divergence_id)
                {
                    v.status = u.status;
                    v.note.clone_from(&u.note);
                }
            }
            _ => {}
        }
    }
    out
}

/// Ids of the claims and divergences already in the ledger.
pub fn recorded_ids(events: &[Event]) -> std::collections::BTreeSet<String> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ClaimExtracted(c) => Some(c.claim_id.clone()),
            EventBody::DivergenceRaised(d) => Some(d.divergence_id.clone()),
            _ => None,
        })
        .collect()
}

/// [`project`] as JSON, the shape of a bundle's `divergences.json`
/// (`wyrd_bundle::DivergenceProjection`).
pub fn project_json(events: &[Event]) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(project(events))
}

#[cfg(test)]
mod tests {
    use wyrd_schema::{DivergenceUpdated, Severity};

    use super::*;

    fn ev(seq: u64, body: EventBody) -> Event {
        Event {
            seq,
            ts_mono_ns: seq,
            ts_wall_ms: 0,
            process: None,
            drops_since_last: 0,
            body,
        }
    }

    #[test]
    fn folds_updates_and_ignores_duplicate_raises() {
        let raised = DivergenceRaised {
            divergence_id: "dv-1".into(),
            rule_id: "D003".into(),
            severity: Severity::Critical,
            claim_id: None,
            evidence_seqs: vec![1],
            confidence_bp: 9500,
            summary: "s".into(),
        };
        let update = |status, note: Option<&str>| {
            EventBody::DivergenceUpdated(DivergenceUpdated {
                divergence_id: "dv-1".into(),
                status,
                note: note.map(str::to_string),
            })
        };
        let events = vec![
            ev(1, EventBody::DivergenceRaised(raised.clone())),
            ev(2, update(DivergenceStatus::Acknowledged, Some("seen"))),
            ev(3, EventBody::DivergenceRaised(raised)),
            ev(
                4,
                update(DivergenceStatus::FalsePositive, Some("planted by the test")),
            ),
        ];
        let v = project(&events);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].seq, 1);
        assert_eq!(v[0].rule_name, "secret-read-then-egress");
        assert_eq!(v[0].status, DivergenceStatus::FalsePositive);
        assert_eq!(v[0].note.as_deref(), Some("planted by the test"));
        assert_eq!(recorded_ids(&events).len(), 1);
    }
}
