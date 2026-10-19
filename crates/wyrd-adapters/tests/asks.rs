//! Pre-execution decisions from a policy, and `ask` answered by a human.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::MemSink;
use serde_json::{Value, json};
use wyrd_adapters::asks::Asks;
use wyrd_adapters::{Ingest, Request, Response, StaticPolicy, ToolPolicy};
use wyrd_schema::{CaptureMode, Decision, EventBody};

const TOKEN: &str = "0123456789abcdef";

/// Asks about `curl`, denies `rm`, allows the rest; records what it saw.
#[derive(Debug, Default)]
struct Fake {
    seen: Mutex<Vec<(String, String, u32)>>,
}

impl ToolPolicy for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn version(&self) -> &'static str {
        "b3:00"
    }
    fn on_tool_call(
        &self,
        tool: &str,
        args: &Value,
        adapter: &str,
        prior: u32,
    ) -> (Decision, String) {
        self.seen
            .lock()
            .unwrap()
            .push((tool.into(), adapter.into(), prior));
        let cmd = args["command"].as_str().unwrap_or_default();
        if cmd.starts_with("curl") {
            (Decision::Ask, "curl to an unknown host".into())
        } else if cmd.starts_with("rm") {
            (Decision::Deny, "no deleting".into())
        } else {
            (Decision::Allow, "fine".into())
        }
    }
}

fn ingest(policy: Arc<Fake>, timeout: Duration, deny: &[&str]) -> Arc<Ingest<MemSink>> {
    Arc::new(
        Ingest::new(
            MemSink::default(),
            CaptureMode::Cell,
            StaticPolicy {
                deny_tools: deny.iter().map(|s| (*s).to_string()).collect(),
            },
            0,
        )
        .with_policy(policy)
        .with_asks(Arc::new(Asks::new(TOKEN.into(), timeout))),
    )
}

fn bash(id: &str, command: &str) -> Request {
    Request::Hook {
        adapter: "claude-code".into(),
        payload: json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": id, "tool_input": {"command": command}}),
    }
}

fn decision(r: &Response) -> (Decision, String) {
    match r {
        Response::Decision { decision, reason } => (*decision, reason.clone().unwrap_or_default()),
        other => panic!("not a decision: {other:?}"),
    }
}

fn guards(i: &Ingest<MemSink>) -> Vec<(Decision, String)> {
    i.sink()
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            EventBody::GuardDecision(g) => Some((g.decision, g.policy_id.clone())),
            _ => None,
        })
        .collect()
}

fn wait_for_ask(i: &Ingest<MemSink>) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Response::Asks { asks } = i.handle(Request::Asks {
            token: TOKEN.into(),
        }) && let Some(a) = asks.first()
        {
            assert_eq!(a.tool, "Bash");
            assert_eq!(a.reason, "curl to an unknown host");
            assert_eq!(a.policy_id, "fake");
            assert!(a.expires_in_ms > 0);
            return a.id;
        }
        assert!(Instant::now() < deadline, "no pending ask");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn policy_allows_and_denies() {
    let p = Arc::new(Fake::default());
    let i = ingest(p.clone(), Duration::from_secs(5), &[]);
    assert_eq!(
        decision(&i.handle(bash("t1", "pnpm test"))).0,
        Decision::Allow
    );
    let (d, reason) = decision(&i.handle(bash("t2", "rm -rf build")));
    assert_eq!((d, reason.as_str()), (Decision::Deny, "no deleting"));
    assert_eq!(
        guards(&i),
        vec![
            (Decision::Allow, "fake".into()),
            (Decision::Deny, "fake".into())
        ]
    );
    let denied = i.sink().events.lock().unwrap().iter().any(
        |e| matches!(e, EventBody::ToolCallDenied(d) if d.policy_id == "fake" && d.call_id == "t2"),
    );
    assert!(denied);
    // The policy learns how many calls were denied before.
    let _ = i.handle(bash("t3", "ls"));
    let seen = p.seen.lock().unwrap().clone();
    assert_eq!(seen.iter().map(|s| s.2).collect::<Vec<_>>(), vec![0, 0, 1]);
    assert!(seen.iter().all(|s| s.1 == "claude-code"));
}

#[test]
fn static_deny_list_comes_first() {
    let p = Arc::new(Fake::default());
    let i = ingest(p.clone(), Duration::from_secs(5), &["Bash"]);
    let (d, _) = decision(&i.handle(bash("t1", "ls")));
    assert_eq!(d, Decision::Deny);
    assert_eq!(guards(&i), vec![(Decision::Deny, StaticPolicy::ID.into())]);
    assert!(p.seen.lock().unwrap().is_empty());
}

#[test]
fn ask_waits_for_the_human_and_records_both_decisions() {
    for allow in [true, false] {
        let i = ingest(Arc::new(Fake::default()), Duration::from_secs(10), &[]);
        let worker = {
            let i = i.clone();
            std::thread::spawn(move || i.handle(bash("t1", "curl https://evil.example")))
        };
        let id = wait_for_ask(&i);
        // The call and the ask are already in the ledger while it waits.
        assert_eq!(guards(&i), vec![(Decision::Ask, "fake".into())]);
        let r = i.handle(Request::Answer {
            token: TOKEN.into(),
            id,
            allow,
            by: "cli".into(),
        });
        assert_eq!(r, Response::Ok);
        let (d, reason) = decision(&worker.join().unwrap());
        let want = if allow {
            Decision::Allow
        } else {
            Decision::Deny
        };
        assert_eq!(d, want);
        if !allow {
            assert!(reason.starts_with("denied by the user (cli"), "{reason}");
        }
        assert_eq!(
            guards(&i),
            vec![(Decision::Ask, "fake".into()), (want, "fake".into())]
        );
        // Answered asks are gone.
        assert_eq!(
            i.handle(Request::Asks {
                token: TOKEN.into()
            }),
            Response::Asks { asks: Vec::new() }
        );
        assert!(matches!(
            i.handle(Request::Answer {
                token: TOKEN.into(),
                id,
                allow,
                by: "cli".into()
            }),
            Response::Error { .. }
        ));
    }
}

#[test]
fn unanswered_ask_is_denied_after_the_timeout() {
    let i = ingest(Arc::new(Fake::default()), Duration::from_millis(200), &[]);
    let t = Instant::now();
    let (d, reason) = decision(&i.handle(bash("t1", "curl https://evil.example")));
    assert_eq!(d, Decision::Deny);
    assert!(reason.starts_with("no answer within"), "{reason}");
    assert!(t.elapsed() >= Duration::from_millis(200));
}

#[test]
fn asks_need_the_control_token() {
    let i = ingest(Arc::new(Fake::default()), Duration::from_secs(10), &[]);
    let worker = {
        let i = i.clone();
        std::thread::spawn(move || i.handle(bash("t1", "curl https://evil.example")))
    };
    let id = wait_for_ask(&i);
    for token in ["", "0123456789abcdee", "0123456789abcdef0"] {
        assert!(matches!(
            i.handle(Request::Asks {
                token: token.into()
            }),
            Response::Error { .. }
        ));
        assert!(matches!(
            i.handle(Request::Answer {
                token: token.into(),
                id,
                allow: true,
                by: "agent".into()
            }),
            Response::Error { .. }
        ));
    }
    i.handle(Request::Answer {
        token: TOKEN.into(),
        id,
        allow: false,
        by: "cli".into(),
    });
    assert_eq!(decision(&worker.join().unwrap()).0, Decision::Deny);
}

#[test]
fn without_an_ask_registry_ask_goes_to_the_agent() {
    let i = Ingest::new(
        MemSink::default(),
        CaptureMode::Cell,
        StaticPolicy::default(),
        0,
    )
    .with_policy(Arc::new(Fake::default()));
    let (d, reason) = decision(&i.handle(bash("t1", "curl x")));
    assert_eq!(
        (d, reason.as_str()),
        (Decision::Ask, "curl to an unknown host")
    );
    assert!(matches!(
        i.handle(Request::Asks {
            token: TOKEN.into()
        }),
        Response::Error { .. }
    ));
}
