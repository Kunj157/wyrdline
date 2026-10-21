#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Rewind requests: recorded without touching the Cell, carried out only
//! when confirmed, never twice, and refused once the ledger is sealed.

use std::collections::BTreeMap;
use std::fs;

use wyrd_cell::requests::{MAX_PENDING, MAX_REASON, RequestStatus};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{CellError, DataDir, Session};

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
}

/// A Cell-mode session with checkpoint c1 (a.txt edited).
fn env() -> (Env, Session) {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
    fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["true".into()],
            observe: false,
            name: None,
            agent_kind: "test".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    fs::write(s.dir().upper("a").join("a.txt"), "edited\n").unwrap();
    s.checkpoint(None).unwrap();
    (Env { _tmp: tmp, data }, s)
}

fn kinds(s: &Session) -> Vec<&'static str> {
    wyrd_ledger::Ledger::open_read_only(s.dir().ledger())
        .unwrap()
        .events_after(0, usize::MAX)
        .unwrap()
        .iter()
        .map(wyrd_schema::Event::kind)
        .collect()
}

#[test]
fn a_request_changes_nothing_until_confirmed() {
    let (env, mut session) = env();
    let req = session
        .request_rewind("c1", "the edit was wrong", "mcp")
        .unwrap();
    assert_eq!(req.status, RequestStatus::Pending);
    assert_eq!(req.checkpoint_id, "c1");
    // Recorded, but no branch, no rewind.
    let reopened = Session::open(&env.data, &session.record().id).unwrap();
    assert_eq!(reopened.record().branches.len(), 1);
    assert_eq!(reopened.record().current_branch, "a");
    let seen = kinds(&reopened);
    assert_eq!(seen.last(), Some(&"meta.rewind.requested"));
    assert!(!seen.contains(&"state.checkpoint.rewound"));
    assert_eq!(reopened.rewind_requests().unwrap(), vec![req.clone()]);

    let branch = session.confirm_rewind(req.id, "cli").unwrap();
    assert_eq!(branch.forked_from.as_deref(), Some("c1"));
    assert_eq!(session.record().current_branch, branch.id);
    let seen = kinds(&session);
    let confirmed = seen
        .iter()
        .position(|k| *k == "meta.rewind.confirmed")
        .unwrap();
    let rewound = seen
        .iter()
        .position(|k| *k == "state.checkpoint.rewound")
        .unwrap();
    assert!(
        confirmed < rewound,
        "the decision precedes the rewind: {seen:?}"
    );
    let after = session.rewind_requests().unwrap();
    assert_eq!(after[0].status, RequestStatus::Confirmed);
    assert_eq!(after[0].decided_by.as_deref(), Some("cli"));

    // A decided request cannot be decided again.
    assert!(matches!(
        session.confirm_rewind(req.id, "cli"),
        Err(CellError::Busy(_))
    ));
    assert!(matches!(
        session.reject_rewind(req.id, "cli", None),
        Err(CellError::Busy(_))
    ));
    assert_eq!(session.record().branches.len(), 2, "confirmed exactly once");
}

#[test]
fn a_rejected_request_never_rewinds() {
    let (_e, mut s) = env();
    let r = s.request_rewind("c1", "please", "mcp").unwrap();
    let rejected = s
        .reject_rewind(r.id, "studio", Some("keep going".into()))
        .unwrap();
    assert_eq!(rejected.status, RequestStatus::Rejected);
    assert_eq!(rejected.note.as_deref(), Some("keep going"));
    assert!(matches!(
        s.confirm_rewind(r.id, "cli"),
        Err(CellError::Busy(_))
    ));
    assert_eq!(s.record().branches.len(), 1);
    assert!(!kinds(&s).contains(&"state.checkpoint.rewound"));
}

#[test]
fn unknown_checkpoints_and_requests_are_errors() {
    let (_e, mut s) = env();
    assert!(matches!(
        s.request_rewind("c9", "x", "mcp"),
        Err(CellError::NotFound(_))
    ));
    // Only checkpoint ids: branch names and `head` are not requests.
    assert!(matches!(
        s.request_rewind("head", "x", "mcp"),
        Err(CellError::NotFound(_))
    ));
    assert!(matches!(
        s.confirm_rewind(1, "cli"),
        Err(CellError::NotFound(_))
    ));
    assert!(matches!(
        s.reject_rewind(999, "cli", None),
        Err(CellError::NotFound(_))
    ));
    assert!(matches!(
        s.request_rewind("c1", &"x".repeat(MAX_REASON + 1), "mcp"),
        Err(CellError::Invalid(_))
    ));
    assert!(matches!(
        s.request_rewind("c1", "  ", "mcp"),
        Err(CellError::Invalid(_))
    ));
    assert_eq!(s.rewind_requests().unwrap(), Vec::new());
}

#[test]
fn pending_requests_are_bounded() {
    let (_e, mut s) = env();
    for i in 0..MAX_PENDING {
        s.request_rewind("c1", &format!("ask {i}"), "mcp").unwrap();
    }
    assert!(matches!(
        s.request_rewind("c1", "one more", "mcp"),
        Err(CellError::Busy(_))
    ));
    let first = s.rewind_requests().unwrap()[0].id;
    s.reject_rewind(first, "cli", None).unwrap();
    s.request_rewind("c1", "room again", "mcp").unwrap();
}

#[test]
fn sealed_sessions_take_no_requests() {
    let (_e, mut s) = env();
    let r = s.request_rewind("c1", "x", "mcp").unwrap();
    s.close("done").unwrap();
    assert!(matches!(
        s.request_rewind("c1", "x", "mcp"),
        Err(CellError::Busy(_))
    ));
    assert!(matches!(
        s.confirm_rewind(r.id, "cli"),
        Err(CellError::Busy(_))
    ));
    assert_eq!(s.record().branches.len(), 1);
    assert!(
        kinds(&s)
            .iter()
            .all(|k| *k != "meta.rewind.confirmed" && *k != "state.checkpoint.rewound")
    );
    // The request is still listed (pending, never carried out).
    assert_eq!(
        s.rewind_requests().unwrap()[0].status,
        RequestStatus::Pending
    );
}
