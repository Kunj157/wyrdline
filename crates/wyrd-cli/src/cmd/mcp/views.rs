//! What the `wyrd mcp` tools answer, computed from one session's data
//! directory. Used directly by `wyrd mcp --session` outside a Cell, and by
//! `wyrd run` to answer the same queries on the intent socket for an
//! agent inside a Cell, which cannot read its own ledger.

use serde_json::{Value, json};
use wyrd_adapters::{SessionQuery, SessionViews};
use wyrd_bundle::redact::{Level, redact_str};
use wyrd_cell::{DataDir, Session};

/// What a rewind request returns to the agent.
pub const PENDING: &str = "pending human confirmation";

/// Answer `query` for `session` (an id or unique prefix).
pub fn answer(data: &DataDir, session: &str, query: &SessionQuery) -> anyhow::Result<Value> {
    match query {
        SessionQuery::ListCheckpoints => {
            let s = Session::open(data, session)?;
            let r = s.record();
            Ok(json!({
                "current_branch": r.current_branch,
                "branches": r.branches,
                "checkpoints": r.checkpoints,
            }))
        }
        SessionQuery::ChangesSince { checkpoint_id } => {
            let s = Session::open(data, session)?;
            let from = s.resolve(checkpoint_id)?;
            let to = s.resolve("head")?;
            let changes = s.diff(&from.id, &to.id)?;
            Ok(json!({ "from": from.id, "to": to.id, "changes": changes }))
        }
        SessionQuery::ExplainEffect { seq } => {
            let events = crate::cmd::graph::load_from(data, session)?;
            crate::cmd::graph::explanation(&events, *seq)
        }
        SessionQuery::ListDivergences => {
            let events = crate::cmd::graph::load_from(data, session)?;
            Ok(json!({ "divergences": wyrd_divergence::view::project(&events) }))
        }
        SessionQuery::RequestRewind {
            checkpoint_id,
            reason,
        } => {
            let mut s = Session::open(data, session)?;
            let reason = redact_str(reason, Level::Standard);
            let request = s.request_rewind(checkpoint_id, &reason, "mcp")?;
            let id = s.record().id.clone();
            Ok(json!({
                "status": PENDING,
                "request": request,
                "message": format!(
                    "Nothing was rewound. A human must confirm request {} \
                     (`wyrd rewind {id} --confirm {}` or Studio) before the session is \
                     rewound to {}.",
                    request.id, request.id, request.checkpoint_id
                ),
            }))
        }
    }
}

/// The intent socket's [`SessionViews`] for a running session.
#[derive(Debug)]
pub struct RunViews {
    pub data: DataDir,
    pub session: String,
}

impl SessionViews for RunViews {
    fn answer(&self, query: &SessionQuery) -> Result<Value, String> {
        answer(&self.data, &self.session, query).map_err(|e| format!("{e:#}"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use wyrd_cell::session::CreateOptions;

    use super::*;

    /// A Cell-mode session with checkpoints c1 and c2.
    fn session() -> (tempfile::TempDir, DataDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), "a\n").unwrap();
        let data = DataDir::new(tmp.path().join("data"));
        let mut s = Session::create(
            &data,
            CreateOptions {
                workspace: ws,
                command: vec!["agent".into()],
                observe: false,
                name: None,
                agent_kind: "generic".into(),
                deny_tools: Vec::new(),
                policy: None,
                kernel_release: "test".into(),
                capabilities: BTreeMap::new(),
            },
        )
        .unwrap();
        s.checkpoint(None).unwrap();
        std::fs::write(s.dir().upper("a").join("b.txt"), "new\n").unwrap();
        s.checkpoint(None).unwrap();
        let id = s.record().id.clone();
        (tmp, data, id)
    }

    #[test]
    fn read_only_views_change_nothing() {
        let (_tmp, data, id) = session();
        let views = RunViews {
            data: data.clone(),
            session: id.clone(),
        };
        let cps = views.answer(&SessionQuery::ListCheckpoints).unwrap();
        assert_eq!(cps["checkpoints"].as_array().unwrap().len(), 2);
        let ch = views
            .answer(&SessionQuery::ChangesSince {
                checkpoint_id: "c1".into(),
            })
            .unwrap();
        assert_eq!(ch["changes"][0]["path"], "b.txt");
        let dv = views.answer(&SessionQuery::ListDivergences).unwrap();
        assert!(dv["divergences"].is_array());
        let err = views
            .answer(&SessionQuery::ChangesSince {
                checkpoint_id: "c9".into(),
            })
            .unwrap_err();
        assert!(err.contains("c9"), "{err}");
        let err = views
            .answer(&SessionQuery::ExplainEffect { seq: 4242 })
            .unwrap_err();
        assert!(err.contains("4242"), "{err}");
        let s = Session::open(&data, &id).unwrap();
        assert_eq!(s.record().branches.len(), 1);
        assert_eq!(s.rewind_requests().unwrap(), Vec::new());
    }

    #[test]
    fn a_rewind_request_is_recorded_redacted_and_rewinds_nothing() {
        let (_tmp, data, id) = session();
        let key = format!("AKIA{}", "Q".repeat(16));
        let v = answer(
            &data,
            &id,
            &SessionQuery::RequestRewind {
                checkpoint_id: "c1".into(),
                reason: format!("broke the build, leaked {key}"),
            },
        )
        .unwrap();
        assert_eq!(v["status"], PENDING);
        assert_eq!(v["request"]["status"], "pending");
        let s = Session::open(&data, &id).unwrap();
        assert_eq!(s.record().current_branch, "a");
        assert_eq!(s.record().branches.len(), 1);
        let reqs = s.rewind_requests().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].source, "mcp");
        assert!(!reqs[0].reason.contains(&key), "{}", reqs[0].reason);
    }
}
