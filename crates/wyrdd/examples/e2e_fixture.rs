//! Builds a data directory for the Studio end-to-end tests, without
//! privileges: a Cell session whose upper layer is written directly (as
//! overlayfs would leave it), four checkpoints, and a synthetic agent run
//! in the ledger (tool calls, kernel effects, a denied connect, and a
//! write outside the Cell).
//!
//! Usage: `e2e_fixture <data-dir> <workspace-dir>`

use std::collections::BTreeMap;
use std::path::PathBuf;

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{
    CallStatus, Event, EventBody, FilePath, FileWrite, IntentSource, Message, NetConnect,
    NetVerdict, ProcessRef, Role, ToolCallFinished, ToolCallStarted,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let data = DataDir::new(PathBuf::from(args.next().ok_or("data dir")?));
    let ws = PathBuf::from(args.next().ok_or("workspace dir")?);
    std::fs::create_dir_all(&ws)?;
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
    println!("{}", s.record().id);
    Ok(())
}
