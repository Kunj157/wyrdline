//! Builds a data directory for the Studio end-to-end tests, without
//! privileges: a Cell session whose upper layer is written directly (as
//! overlayfs would leave it), four checkpoints, and a synthetic agent run
//! in the ledger (tool calls, kernel effects, a denied connect, and a
//! write outside the Cell).
//!
//! It also creates a second session left in the `recording` state for the
//! live-mode spec, which adds events to it while the page is open.
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
    CallStatus, Event, EventBody, FilePath, FileWrite, IntentSource, Message, NetConnect,
    NetVerdict, ProcessRef, Role, ToolCallFinished, ToolCallStarted,
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
    let mut rec = wyrd_cell::state::load(s.dir())?;
    rec.status = wyrd_schema::SessionState::Recording;
    wyrd_cell::state::save(s.dir(), &rec)?;
    Ok(rec.id)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|a| a == "append") {
        args.next();
        return append(args);
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
    ledger.append(events)?;
    // The test harness reads the session ids from stdout.
    writeln!(std::io::stdout(), "{}\n{live}", s.record().id)?;
    Ok(())
}
