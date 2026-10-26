//! Session views on the intent socket: read-only queries and rewind
//! requests answered by the session, never a command.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::MemSink;
use serde_json::{Value, json};
use wyrd_adapters::protocol::call;
use wyrd_adapters::{Ingest, Request, Response, SessionQuery, SessionViews, StaticPolicy, server};
use wyrd_schema::CaptureMode;

/// Answers every query with its own JSON; fails on checkpoint `c9`.
#[derive(Debug, Default)]
struct Fake {
    seen: Mutex<Vec<SessionQuery>>,
}

impl SessionViews for Fake {
    fn answer(&self, query: &SessionQuery) -> Result<Value, String> {
        self.seen.lock().unwrap().push(query.clone());
        match query {
            SessionQuery::ChangesSince { checkpoint_id } if checkpoint_id == "c9" => {
                Err("not found: checkpoint c9".into())
            }
            q => Ok(serde_json::to_value(q).unwrap()),
        }
    }
}

fn ingest(views: Option<Arc<Fake>>) -> Ingest<MemSink> {
    let i = Ingest::new(
        MemSink::default(),
        CaptureMode::Cell,
        StaticPolicy::default(),
        0,
    );
    match views {
        Some(v) => i.with_views(v),
        None => i,
    }
}

#[test]
fn queries_have_a_stable_wire_format() {
    let req: Request = serde_json::from_value(json!({
        "type": "session",
        "query": {"query": "request_rewind", "checkpoint_id": "c2", "reason": "broke it"}
    }))
    .unwrap();
    assert_eq!(
        req,
        Request::Session {
            query: SessionQuery::RequestRewind {
                checkpoint_id: "c2".into(),
                reason: "broke it".into(),
            }
        }
    );
    for q in [
        json!({"query": "list_checkpoints"}),
        json!({"query": "changes_since", "checkpoint_id": "c1"}),
        json!({"query": "explain_effect", "seq": 12}),
        json!({"query": "list_divergences"}),
    ] {
        let parsed: SessionQuery = serde_json::from_value(q.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), q);
    }
    // There is no query that confirms, rejects, or rewinds.
    for q in ["confirm_rewind", "reject_rewind", "rewind", "apply"] {
        assert!(serde_json::from_value::<SessionQuery>(json!({"query": q})).is_err());
    }
}

#[test]
fn views_answer_queries_and_errors_stay_errors() {
    let fake = Arc::new(Fake::default());
    let i = ingest(Some(Arc::clone(&fake)));
    let r = i.handle(Request::Session {
        query: SessionQuery::ExplainEffect { seq: 7 },
    });
    assert_eq!(
        r,
        Response::View {
            result: json!({"query": "explain_effect", "seq": 7})
        }
    );
    let r = i.handle(Request::Session {
        query: SessionQuery::ChangesSince {
            checkpoint_id: "c9".into(),
        },
    });
    assert_eq!(
        r,
        Response::Error {
            message: "not found: checkpoint c9".into()
        }
    );
    assert_eq!(fake.seen.lock().unwrap().len(), 2);
}

#[test]
fn a_session_without_views_says_so() {
    let r = ingest(None).handle(Request::Session {
        query: SessionQuery::ListCheckpoints,
    });
    assert!(
        matches!(&r, Response::Error { message } if message.contains("views")),
        "{r:?}"
    );
}

#[test]
fn views_over_the_real_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("i.sock");
    let listener = server::bind(&path).unwrap();
    let fake = Arc::new(Fake::default());
    let i = Arc::new(ingest(Some(Arc::clone(&fake))));
    std::thread::spawn(move || server::serve(&listener, &i));
    let r = call(
        &path,
        &Request::Session {
            query: SessionQuery::ListDivergences,
        },
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        r,
        Response::View {
            result: json!({"query": "list_divergences"})
        }
    );
}
