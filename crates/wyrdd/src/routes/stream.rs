//! `GET /v1/sessions/{sid}/stream`: ledger events as Server-Sent Events,
//! resumable with `Last-Event-ID`.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::Stream;
use futures_util::stream;
use serde::Deserialize;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::SessionState;

use super::sessions::{blocking, open};
use crate::AppState;
use crate::problem::Problem;

/// How often the ledger is polled for new events.
pub const POLL: Duration = Duration::from_millis(250);
/// Most events sent per poll; the next poll continues, so a slow reader
/// holds the stream back instead of growing a buffer.
pub const BATCH: usize = 500;
/// Interval of the keep-alive comment.
pub const HEARTBEAT: Duration = Duration::from_secs(15);

#[derive(Debug, Default, Deserialize)]
pub struct StreamQuery {
    after_seq: Option<u64>,
}

/// The seq to resume after: `Last-Event-ID` wins over `?after_seq=`.
pub fn resume_point(headers: &HeaderMap, query: Option<u64>) -> Result<u64, Problem> {
    match headers.get("last-event-id") {
        None => Ok(query.unwrap_or(0)),
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| {
                Problem::new(
                    StatusCode::BAD_REQUEST,
                    "invalid-last-event-id",
                    "Last-Event-ID must be a ledger seq",
                )
            }),
    }
}

/// One poll: events after `after` (at most [`BATCH`]) and whether the
/// session has ended.
fn poll(data: &DataDir, sid: &str, after: u64) -> Result<(Vec<SseEvent>, u64, bool), Problem> {
    let s = Session::open(data, sid)?;
    let ended = matches!(
        s.record().status,
        SessionState::Closed | SessionState::Failed
    );
    let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
    let batch = ledger.events_after(after, BATCH)?;
    let mut last = after;
    let mut out = Vec::with_capacity(batch.len());
    for ev in &batch {
        last = ev.seq;
        out.push(
            SseEvent::default()
                .id(ev.seq.to_string())
                .event("ledger")
                .json_data(ev)
                .map_err(|e| Problem::internal(e.to_string()))?,
        );
    }
    // Ended only once nothing is left to send.
    Ok((out, last, ended && batch.is_empty()))
}

struct Cursor {
    data: DataDir,
    sid: String,
    after: u64,
    pending: std::vec::IntoIter<SseEvent>,
    first: bool,
    done: bool,
}

fn events(c: Cursor) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    stream::unfold(c, |mut c| async move {
        loop {
            if let Some(ev) = c.pending.next() {
                return Some((Ok(ev), c));
            }
            if c.done {
                return None;
            }
            if !c.first {
                tokio::time::sleep(POLL).await;
            }
            c.first = false;
            let (data, sid, after) = (c.data.clone(), c.sid.clone(), c.after);
            match blocking(move || poll(&data, &sid, after)).await {
                Ok((batch, last, ended)) => {
                    c.after = last;
                    if ended {
                        c.done = true;
                        let end = SseEvent::default().event("end").data(last.to_string());
                        return Some((Ok(end), c));
                    }
                    c.pending = batch.into_iter();
                }
                Err(p) => {
                    tracing::warn!(session = %c.sid, error = ?p.detail, "stream poll failed");
                    c.done = true;
                    let err = SseEvent::default()
                        .event("error")
                        .data(p.detail.unwrap_or(p.title));
                    return Some((Ok(err), c));
                }
            }
        }
    })
}

pub async fn stream(
    State(st): State<Arc<AppState>>,
    Path(sid): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, Problem> {
    let after = resume_point(&headers, q.after_seq)?;
    let data = st.data.clone();
    // Resolve the session (and a prefix to the full id) before streaming,
    // so an unknown session is a plain 404.
    let sid = blocking(move || Ok(open(&data, &sid)?.record().id.clone())).await?;
    tracing::info!(session = %sid, after, "stream opened");
    let cursor = Cursor {
        data: st.data.clone(),
        sid,
        after,
        pending: Vec::new().into_iter(),
        first: true,
        done: false,
    };
    Ok(Sse::new(events(cursor)).keep_alive(KeepAlive::new().interval(HEARTBEAT)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_event_id_wins_and_must_be_a_seq() {
        let mut h = HeaderMap::new();
        assert_eq!(resume_point(&h, None).unwrap(), 0);
        assert_eq!(resume_point(&h, Some(7)).unwrap(), 7);
        h.insert("last-event-id", "42".parse().unwrap());
        assert_eq!(resume_point(&h, Some(7)).unwrap(), 42);
        h.insert("last-event-id", "x".parse().unwrap());
        assert!(resume_point(&h, None).is_err());
    }
}
