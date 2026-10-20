//! Rewind requests: the agent asks (through `wyrd mcp`) to be rewound to a
//! checkpoint, and a human confirms or rejects the request. The state is
//! projected from the ledger, so pending requests survive restarts and
//! need no running process to be listed or answered.

use schemars::JsonSchema;
use serde::Serialize;
use ts_rs::TS;
use wyrd_schema::{Event, EventBody};

/// Longest reason accepted with a request (characters).
pub const MAX_REASON: usize = 2_000;

/// Longest note accepted with a rejection (characters).
pub const MAX_NOTE: usize = 2_000;

/// Undecided requests a session holds at most; further requests are
/// refused until a human answers, so an agent cannot flood the ledger.
pub const MAX_PENDING: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    /// Waiting for a human.
    Pending,
    /// A human confirmed; the rewind follows it in the ledger.
    Confirmed,
    /// A human rejected; nothing was changed.
    Rejected,
}

/// One rewind request with its decision, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema, TS)]
pub struct RewindRequest {
    /// Request id: the `seq` of its `meta.rewind.requested` event.
    #[ts(type = "number")]
    pub id: u64,
    pub checkpoint_id: String,
    /// The agent's reason (redacted).
    pub reason: String,
    /// Surface the request came through, e.g. `mcp`.
    pub source: String,
    /// Wall-clock milliseconds of the request.
    #[ts(type = "number")]
    pub requested_ms: i64,
    pub status: RequestStatus,
    /// `cli` or `studio` once decided.
    pub decided_by: Option<String>,
    /// The rejection note, if one was given.
    pub note: Option<String>,
}

fn decide(
    out: &mut [RewindRequest],
    id: u64,
    status: RequestStatus,
    by: &str,
    note: Option<&String>,
) {
    if let Some(r) = out
        .iter_mut()
        .find(|r| r.id == id && r.status == RequestStatus::Pending)
    {
        r.status = status;
        r.decided_by = Some(by.to_string());
        r.note = note.cloned();
    }
}

/// Every rewind request in `events` (ledger order) with its decision. A
/// decision naming an unknown or already decided request is ignored: the
/// first decision wins.
pub fn project(events: &[Event]) -> Vec<RewindRequest> {
    let mut out = Vec::new();
    for ev in events {
        match &ev.body {
            EventBody::RewindRequested(r) => out.push(RewindRequest {
                id: ev.seq,
                checkpoint_id: r.checkpoint_id.clone(),
                reason: r.reason.clone(),
                source: r.source.clone(),
                requested_ms: ev.ts_wall_ms,
                status: RequestStatus::Pending,
                decided_by: None,
                note: None,
            }),
            EventBody::RewindConfirmed(c) => {
                decide(
                    &mut out,
                    c.request_seq,
                    RequestStatus::Confirmed,
                    &c.by,
                    None,
                );
            }
            EventBody::RewindRejected(r) => decide(
                &mut out,
                r.request_seq,
                RequestStatus::Rejected,
                &r.by,
                r.note.as_ref(),
            ),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrd_schema::{RewindConfirmed, RewindRejected, RewindRequested};

    fn ev(seq: u64, body: EventBody) -> Event {
        Event {
            seq,
            ts_mono_ns: 0,
            ts_wall_ms: i64::try_from(seq).unwrap_or(0) * 10,
            process: None,
            drops_since_last: 0,
            body,
        }
    }

    fn requested(cp: &str) -> EventBody {
        EventBody::RewindRequested(RewindRequested {
            checkpoint_id: cp.into(),
            reason: "r".into(),
            source: "mcp".into(),
        })
    }

    #[test]
    fn decisions_apply_once_and_only_to_known_requests() {
        let events = vec![
            ev(3, requested("c1")),
            ev(4, requested("c2")),
            ev(
                5,
                EventBody::RewindRejected(RewindRejected {
                    request_seq: 3,
                    by: "cli".into(),
                    note: Some("no".into()),
                }),
            ),
            // Second decision for 3 and one for a seq that is no request.
            ev(
                6,
                EventBody::RewindConfirmed(RewindConfirmed {
                    request_seq: 3,
                    checkpoint_id: "c1".into(),
                    by: "studio".into(),
                }),
            ),
            ev(
                7,
                EventBody::RewindConfirmed(RewindConfirmed {
                    request_seq: 5,
                    checkpoint_id: "c1".into(),
                    by: "studio".into(),
                }),
            ),
        ];
        let p = project(&events);
        assert_eq!(p.len(), 2);
        assert_eq!(
            (p[0].id, p[0].status, p[0].decided_by.as_deref()),
            (3, RequestStatus::Rejected, Some("cli"))
        );
        assert_eq!(p[0].note.as_deref(), Some("no"));
        assert_eq!(p[0].requested_ms, 30);
        assert_eq!((p[1].id, p[1].status), (4, RequestStatus::Pending));
    }

    #[test]
    fn a_decision_before_its_request_is_ignored() {
        let events = vec![
            ev(
                2,
                EventBody::RewindConfirmed(RewindConfirmed {
                    request_seq: 3,
                    checkpoint_id: "c1".into(),
                    by: "cli".into(),
                }),
            ),
            ev(3, requested("c1")),
        ];
        assert_eq!(project(&events)[0].status, RequestStatus::Pending);
    }
}
