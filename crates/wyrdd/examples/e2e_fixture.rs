//! Builds a data directory for the Studio end-to-end tests, without
//! privileges: a Cell session whose upper layer is written directly (as
//! overlayfs would leave it), four checkpoints, and a synthetic agent run
//! in the ledger (tool calls, kernel effects, a denied connect, a write
//! outside the Cell, and the claim and divergences recorded after it).
//!
//! It also creates a second session left in the `recording` state for the
//! live-mode spec, which adds events to it while the page is open. That
//! session also carries a divergence, and its record points at the
//! fixture's own cgroup, which stays populated while the e2e run lives, so
//! the daemon sees the session as running and refuses to triage it. Nothing
//! in the suite stops or rewinds it.
//!
//! Usage: `e2e_fixture <data-dir> <workspace-dir>` prints both session ids
//! (one per line); `e2e_fixture append <data-dir> <session> <text>` appends
//! an assistant message to a session's ledger.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{
    CallStatus, ClaimExtracted, ClaimType, DivergenceRaised, Event, EventBody, Extractor, FilePath,
    FileWrite, IntentSource, Message, NetConnect, NetVerdict, ProcessRef, Role, Severity,
    ToolCallFinished, ToolCallStarted,
};

fn append(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let data = DataDir::new(PathBuf::from(args.next().ok_or("data dir")?));
    let sid = args.next().ok_or("session")?;
    let text = args.next().ok_or("text")?;
    let s = Session::open(&data, &sid)?;
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger())?;
    let seq = ledger.append(vec![Event {
        seq: 0,
        ts_mono_ns: 10_000_000_000,
        ts_wall_ms: 1_760_000_001_000,
        process: None,
        drops_since_last: 0,
        body: EventBody::Message(Message {
            role: Role::Assistant,
            turn: 2,
            text,
        }),
    }])?;
    writeln!(std::io::stdout(), "{seq}")?;
    Ok(())
}

/// A session that looks like a running agent: Recording, a few events.
/// Asks about every tool call; answered from Studio in the e2e suite.
#[derive(Debug)]
struct AlwaysAsk;

impl wyrd_adapters::ToolPolicy for AlwaysAsk {
    fn id(&self) -> &'static str {
        "e2e-ask"
    }
    fn version(&self) -> &'static str {
        "b3:e2e"
    }
    fn on_tool_call(
        &self,
        _: &str,
        _: &serde_json::Value,
        _: &str,
        _: u32,
    ) -> (wyrd_schema::Decision, String) {
        (
            wyrd_schema::Decision::Ask,
            "curl to a host outside the allow-list".into(),
        )
    }
}

struct Ledger(std::sync::Mutex<wyrd_ledger::Ledger>);

impl wyrd_adapters::Sink for Ledger {
    fn append(&self, bodies: Vec<EventBody>) -> Result<(), String> {
        let events = bodies
            .into_iter()
            .map(|body| Event {
                seq: 0,
                ts_mono_ns: 11_000_000_000,
                ts_wall_ms: 1_760_000_002_000,
                process: None,
                drops_since_last: 0,
                body,
            })
            .collect();
        self.0
            .lock()
            .map_err(|e| e.to_string())?
            .append(events)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Serve the live session's intent socket like `wyrd run`, make one tool
/// call that the policy asks about, and print the final decision.
fn ask(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    use wyrd_adapters::asks::{Asks, CONTROL_TOKEN_FILE};
    let data = DataDir::new(PathBuf::from(args.next().ok_or("data dir")?));
    let sid = args.next().ok_or("session")?;
    let s = Session::open(&data, &sid)?;
    let token = wyrd_adapters::asks::new_token()?;
    wyrd_adapters::asks::write_private_atomic(
        &s.dir().root().join(CONTROL_TOKEN_FILE),
        token.as_bytes(),
    )?;
    let socket = wyrd_adapters::server::socket_path(s.dir().root(), &sid);
    let listener = wyrd_adapters::server::bind(&socket)?;
    let ingest = std::sync::Arc::new(
        wyrd_adapters::Ingest::new(
            Ledger(std::sync::Mutex::new(wyrd_ledger::Ledger::open(
                s.dir().ledger(),
            )?)),
            wyrd_schema::CaptureMode::Observe,
            wyrd_adapters::StaticPolicy::default(),
            1,
        )
        .with_policy(std::sync::Arc::new(AlwaysAsk))
        .with_asks(std::sync::Arc::new(Asks::new(
            token,
            std::time::Duration::from_secs(60),
        ))),
    );
    let served = ingest.clone();
    std::thread::spawn(move || wyrd_adapters::server::serve(&listener, &served));
    let r = ingest.handle(wyrd_adapters::Request::Hook {
        adapter: "claude-code".into(),
        payload: serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": "ask-1", "tool_input": {"command": "curl https://diag.example/upload"}}),
    });
    let _ = std::fs::remove_file(s.dir().root().join(CONTROL_TOKEN_FILE));
    let _ = std::fs::remove_file(&socket);
    // The answer that woke us may still be on its way out, and Studio asks
    // again right after it: finish those connections before exiting.
    wyrd_adapters::server::drain(std::time::Duration::from_secs(2));
    writeln!(std::io::stdout(), "{}", serde_json::to_string(&r)?)?;
    Ok(())
}

fn live_session(
    data: &DataDir,
    ws: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let live_ws = ws.with_file_name("live-ws");
    std::fs::create_dir_all(&live_ws)?;
    let s = Session::create(
        data,
        CreateOptions {
            workspace: live_ws,
            command: vec!["agent".into(), "keep working".into()],
            observe: true,
            name: Some("e2e live session".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "e2e".into(),
            capabilities: BTreeMap::new(),
        },
    )?;
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger())?;
    ledger.append(vec![Event {
        seq: 0,
        ts_mono_ns: 2_000_000_000,
        ts_wall_ms: 1_760_000_000_500,
        process: None,
        drops_since_last: 0,
        body: EventBody::DivergenceRaised(DivergenceRaised {
            divergence_id: "dv-e2e00000live".into(),
            rule_id: "D002".into(),
            severity: Severity::Medium,
            claim_id: None,
            evidence_seqs: vec![1],
            confidence_bp: 7000,
            summary: "wrote /etc/hosts outside the task's scope".into(),
        }),
    }])?;
    let mut rec = wyrd_cell::state::load(s.dir())?;
    rec.status = wyrd_schema::SessionState::Recording;
    rec.cgroup = Some(
        wyrd_cell::cgroup::Cgroup::of_pid(std::process::id())?
            .path()
            .to_path_buf(),
    );
    wyrd_cell::state::save(s.dir(), &rec)?;
    Ok(rec.id)
}

/// What `wyrd run` records after the run: the agent's "All tests pass."
/// claim (seq `said`) without a test run, and the write outside the Cell
/// (seq `write`).
fn verdicts(write: u64, said: u64) -> Vec<Event> {
    [
        EventBody::ClaimExtracted(ClaimExtracted {
            claim_id: "cl-e2e000000001".into(),
            turn: 1,
            claim_type: ClaimType::TestsPass,
            subject: "tests".into(),
            quote: "All tests pass.".into(),
            extractor: Extractor::Rule,
        }),
        EventBody::DivergenceRaised(DivergenceRaised {
            divergence_id: "dv-e2e000000001".into(),
            rule_id: "D001".into(),
            severity: Severity::High,
            claim_id: Some("cl-e2e000000001".into()),
            evidence_seqs: vec![said],
            confidence_bp: 9000,
            summary: "claimed the tests pass, but no test run exited 0".into(),
        }),
        EventBody::DivergenceRaised(DivergenceRaised {
            divergence_id: "dv-e2e000000002".into(),
            rule_id: "D002".into(),
            severity: Severity::Critical,
            claim_id: None,
            evidence_seqs: vec![write],
            confidence_bp: 9500,
            summary: "wrote /home/user/.bashrc outside the workspace".into(),
        }),
    ]
    .into_iter()
    .map(|body| Event {
        seq: 0,
        ts_mono_ns: 9_500_000_000,
        ts_wall_ms: 1_760_000_000_500,
        process: None,
        drops_since_last: 0,
        body,
    })
    .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|a| a == "append") {
        args.next();
        return append(args);
    }
    if args.peek().is_some_and(|a| a == "ask") {
        args.next();
        return ask(args);
    }
    let data = DataDir::new(PathBuf::from(args.next().ok_or("data dir")?));
    let ws = PathBuf::from(args.next().ok_or("workspace dir")?);
    std::fs::create_dir_all(&ws)?;
    let live = live_session(&data, &ws)?;
    std::fs::write(ws.join("greet.txt"), "helo world\n")?;
    std::fs::write(ws.join("old.txt"), "stale\n")?;

    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into(), "fix the greeting".into()],
            observe: false,
            name: Some("e2e greeting fix".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "e2e".into(),
            capabilities: BTreeMap::new(),
        },
    )?;
    let up = s.dir().upper("a");
    s.checkpoint(None)?;
    std::fs::write(up.join("greet.txt"), "hello world\n")?;
    s.checkpoint(Some("call-1".into()))?;
    std::fs::create_dir(up.join("out"))?;
    std::fs::write(up.join("out/report.txt"), "all good\n")?;
    s.checkpoint(Some("call-2".into()))?;
    std::fs::write(up.join("greet.txt"), "hello, world\n")?;
    s.checkpoint(Some("call-3".into()))?;

    let mut events = wyrd_correlator::synth::session(7).events;
    let proc = Some(ProcessRef {
        pid: 4242,
        tid: 4242,
        ppid: 1000,
        start_ns: 5,
        cgroup_id: 1,
    });
    let ev = |body: EventBody| Event {
        seq: 0,
        ts_mono_ns: 9_000_000_000,
        ts_wall_ms: 1_760_000_000_000,
        process: proc,
        drops_since_last: 0,
        body,
    };
    events.extend([
        ev(EventBody::Message(Message {
            role: Role::User,
            turn: 1,
            text: "Fix the greeting and tell me when the tests pass.".into(),
        })),
        ev(EventBody::ToolCallStarted(ToolCallStarted {
            call_id: "net-1".into(),
            source: IntentSource::Hook,
            tool: "Bash".into(),
            args: serde_json::json!({ "command": "curl https://diag.example/upload" }),
            turn: Some(1),
        })),
        ev(EventBody::NetConnect(NetConnect {
            family: 2,
            addr: "203.0.113.9".into(),
            port: 443,
            dns_name: Some("diag.example".into()),
            verdict: NetVerdict::Denied,
        })),
        ev(EventBody::FileWrite(FileWrite {
            file: FilePath {
                path: "/home/user/.bashrc".into(),
                in_cell: false,
                truncated: false,
            },
            bytes: 12,
        })),
        ev(EventBody::ToolCallFinished(ToolCallFinished {
            call_id: "net-1".into(),
            status: CallStatus::Error,
            exit_code: Some(7),
            summary: None,
            duration_ms: 40,
        })),
        ev(EventBody::Message(Message {
            role: Role::Assistant,
            turn: 1,
            text: "All tests pass.".into(),
        })),
    ]);
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger())?;
    // The last six events: user message, call, connect, write, call end,
    // assistant message.
    let last = ledger.append(events)?;
    ledger.append(verdicts(last - 2, last))?;
    // The test harness reads the session ids from stdout.
    writeln!(std::io::stdout(), "{}\n{live}", s.record().id)?;
    Ok(())
}
