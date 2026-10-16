#![allow(clippy::unwrap_used, clippy::panic, clippy::cast_possible_truncation)]

use wyrd_correlator::{Attribution, correlate};
use wyrd_schema::*;

/// Builder for a synthetic session.
#[derive(Default)]
struct S {
    events: Vec<Event>,
}

const ROOT: (u32, u64) = (100, 1);

impl S {
    fn push(&mut self, ts: u64, process: Option<(u32, u64, u32)>, body: EventBody) -> u64 {
        let seq = self.events.len() as u64 + 1;
        self.events.push(Event {
            seq,
            ts_mono_ns: ts,
            ts_wall_ms: 0,
            process: process.map(|(pid, start, ppid)| ProcessRef {
                pid,
                tid: pid,
                ppid,
                start_ns: start,
                cgroup_id: 9,
            }),
            drops_since_last: 0,
            body,
        });
        seq
    }
    fn call(
        &mut self,
        ts: u64,
        id: &str,
        tool: &str,
        args: serde_json::Value,
        source: IntentSource,
    ) {
        self.push(
            ts,
            None,
            EventBody::ToolCallStarted(ToolCallStarted {
                call_id: id.into(),
                source,
                tool: tool.into(),
                args,
                turn: Some(1),
            }),
        );
    }
    fn done(&mut self, ts: u64, id: &str) {
        self.push(
            ts,
            None,
            EventBody::ToolCallFinished(ToolCallFinished {
                call_id: id.into(),
                status: CallStatus::Ok,
                exit_code: Some(0),
                summary: None,
                duration_ms: 1,
            }),
        );
    }
    fn fork(&mut self, ts: u64, parent: (u32, u64), child: (u32, u64)) {
        self.push(
            ts,
            Some((parent.0, parent.1, 1)),
            EventBody::ProcFork(ProcFork {
                child_pid: child.0,
                child_start_ns: child.1,
            }),
        );
    }
    fn exec(&mut self, ts: u64, p: (u32, u64), ppid: u32, argv: &[&str]) -> u64 {
        self.push(
            ts,
            Some((p.0, p.1, ppid)),
            EventBody::ProcExec(ProcExec {
                filename: format!("/usr/bin/{}", argv[0]),
                argv: argv.iter().map(|a| (*a).to_string()).collect(),
                cwd: Some("/w".into()),
                truncated: false,
            }),
        )
    }
    fn write(&mut self, ts: u64, p: (u32, u64), ppid: u32, path: &str) -> u64 {
        self.push(
            ts,
            Some((p.0, p.1, ppid)),
            EventBody::FileOpenWrite(FileOpenWrite {
                file: FilePath {
                    path: path.into(),
                    in_cell: true,
                    truncated: false,
                },
                flags: 577,
            }),
        )
    }
}

fn session() -> (S, Vec<u64>) {
    let mut s = S::default();
    let mut seqs = Vec::new();
    s.exec(10, ROOT, 1, &["claude"]); // agent root
    // Bash call: sh -c "pnpm test" → node writes a report, leaves a server running.
    s.call(
        100,
        "bash1",
        "Bash",
        serde_json::json!({"command": "pnpm   test"}),
        IntentSource::Hook,
    );
    s.fork(110, ROOT, (200, 110));
    s.exec(120, (200, 110), 100, &["sh", "-c", "pnpm test"]);
    s.fork(130, (200, 110), (201, 130));
    s.exec(135, (201, 130), 200, &["node", "run.js"]);
    seqs.push(s.write(150, (201, 130), 200, "/w/report.xml")); // 0: direct
    s.fork(160, (201, 130), (202, 160));
    s.done(200, "bash1");
    seqs.push(s.write(300, (202, 160), 201, "/w/server.log")); // 1: direct, after the window
    // Edit call performed in-process by the agent.
    s.call(
        400,
        "edit1",
        "Edit",
        serde_json::json!({"file_path": "/w/src/a.rs", "old_string": "x"}),
        IntentSource::Hook,
    );
    seqs.push(s.write(410, ROOT, 1, "/w/src/a.rs")); // 2: direct (file tool)
    seqs.push(s.write(415, ROOT, 1, "/w/src/other.rs")); // 3: window
    s.done(420, "edit1");
    // Two overlapping MCP tool calls.
    s.call(
        500,
        "m1",
        "mcp__fs__write",
        serde_json::json!({}),
        IntentSource::Hook,
    );
    s.call(
        510,
        "m2",
        "mcp__fs__write",
        serde_json::json!({}),
        IntentSource::Hook,
    );
    s.call(511, "g1", "write", serde_json::json!({}), IntentSource::Mcp); // gateway view of m1/m2
    seqs.push(s.write(520, ROOT, 1, "/w/x")); // 4: ambiguous between m1 and m2
    s.done(530, "g1");
    s.done(540, "m1");
    s.done(550, "m2");
    // Outside every window.
    seqs.push(s.write(600, ROOT, 1, "/home/u/.claude.json")); // 5: agent runtime
    seqs.push(s.write(610, (900, 5), 1, "/w/stranger")); // 6: not in the agent tree
    // PID reuse: pid 200 again, different start, inside no window.
    seqs.push(s.write(700, (200, 690), 100, "/w/reused")); // 7
    (s, seqs)
}

#[test]
fn attribution_follows_the_plan_rules() {
    let (s, seq) = session();
    let g = correlate(&s.events);
    let eff = |i: usize| g.effect(seq[i]).unwrap().clone();
    assert_eq!(g.agent_root, Some(100));
    assert_eq!(
        (eff(0).attribution, eff(0).call_id.as_deref()),
        (Attribution::Direct, Some("bash1"))
    );
    assert_eq!(eff(0).confidence_bp, 10_000);
    assert_eq!(
        (eff(1).attribution, eff(1).call_id.as_deref()),
        (Attribution::Direct, Some("bash1")),
        "background process stays attributed"
    );
    assert_eq!(
        (eff(2).attribution, eff(2).call_id.as_deref()),
        (Attribution::Direct, Some("edit1"))
    );
    assert_eq!(
        (eff(3).attribution, eff(3).call_id.as_deref()),
        (Attribution::Window, Some("edit1"))
    );
    assert_eq!(eff(3).confidence_bp, 8_000);
    let amb = eff(4);
    assert_eq!(amb.attribution, Attribution::Ambiguous);
    assert_eq!(amb.call_id, None, "never pick silently");
    assert_eq!(
        amb.candidates,
        vec!["m1", "m2"],
        "the nested gateway call does not compete"
    );
    assert_eq!(amb.confidence_bp, 2_500);
    assert_eq!(eff(5).attribution, Attribution::AgentRuntime);
    assert_eq!(eff(6).attribution, Attribution::Unattributed);
    assert_eq!(
        eff(7).attribution,
        Attribution::Unattributed,
        "a reused pid is a different process"
    );
    // Equal windows: the earliest-started one wins.
    assert_eq!(g.call("g1").unwrap().nested_in.as_deref(), Some("m1"));
    let bash = &g.call("bash1").unwrap().effects;
    assert!(bash.contains(&seq[0]) && bash.contains(&seq[1]), "{bash:?}");
    assert!(!bash.contains(&seq[2]), "the edit belongs to edit1");
    assert_eq!(g.processes.iter().filter(|p| p.pid == 200).count(), 2);
}

#[test]
fn deterministic_and_serialisable() {
    let (s, _) = session();
    let a = correlate(&s.events);
    let b = correlate(&s.events);
    assert_eq!(a, b);
    let json = serde_json::to_string(&a).unwrap();
    let back: wyrd_correlator::Graph = serde_json::from_str(&json).unwrap();
    assert_eq!(back, a);
    let summary = a.summary();
    let direct = a
        .effects
        .iter()
        .filter(|e| e.attribution == Attribution::Direct)
        .count();
    assert_eq!(summary.get("direct"), Some(&direct));
    assert_eq!(summary.values().sum::<usize>(), a.effects.len());
}

#[test]
fn truncated_argv_matches_by_prefix_and_others_do_not() {
    let long = "cargo test --workspace --locked --all-features -- --nocapture --test-threads 1";
    let mut s = S::default();
    s.exec(1, ROOT, 1, &["agent"]);
    s.call(
        10,
        "c",
        "Bash",
        serde_json::json!({"command": long}),
        IntentSource::Hook,
    );
    s.fork(11, ROOT, (5, 11));
    s.exec(12, (5, 11), 100, &["bash", "-c", &long[..63]]); // kernel keeps 63 chars + NUL
    let w = s.write(13, (5, 11), 100, "/w/target/x");
    s.done(20, "c");
    s.call(
        30,
        "d",
        "Bash",
        serde_json::json!({"command": "ls"}),
        IntentSource::Hook,
    );
    s.fork(31, ROOT, (6, 31));
    s.exec(32, (6, 31), 100, &["sh", "-c", "lsblk"]); // not "ls"
    let w2 = s.write(33, (6, 31), 100, "/w/y");
    s.done(40, "d");
    let g = correlate(&s.events);
    assert_eq!(g.effect(w).unwrap().attribution, Attribution::Direct);
    assert_eq!(
        g.effect(w2).unwrap().attribution,
        Attribution::Window,
        "no exact command match → window"
    );
}

#[test]
fn empty_and_intent_only_sessions() {
    assert_eq!(correlate(&[]), wyrd_correlator::Graph::default());
    let mut s = S::default();
    s.call(1, "c", "Read", serde_json::json!({}), IntentSource::Hook);
    let g = correlate(&s.events);
    assert_eq!(g.calls.len(), 1);
    assert_eq!(g.effects.len(), 0);
    assert_eq!(g.agent_root, None);
}

#[test]
fn wyrdline_hook_helpers_are_infrastructure() {
    let mut s = S::default();
    s.exec(1, ROOT, 1, &["claude"]);
    s.call(
        10,
        "c",
        "Bash",
        serde_json::json!({"command": "make"}),
        IntentSource::Hook,
    );
    // PreToolUse hook: the agent forks `wyrd hook claude-code`.
    s.fork(11, ROOT, (7, 11));
    let hook_exec = s.push(
        12,
        Some((7, 11, 100)),
        EventBody::ProcExec(ProcExec {
            filename: "/usr/local/bin/wyrd".into(),
            argv: vec![
                "/usr/local/bin/wyrd".into(),
                "hook".into(),
                "claude-code".into(),
            ],
            cwd: None,
            truncated: false,
        }),
    );
    s.fork(20, ROOT, (8, 20));
    s.exec(21, (8, 20), 100, &["sh", "-c", "make"]);
    let w = s.write(22, (8, 20), 100, "/w/out.o");
    s.done(30, "c");
    let g = correlate(&s.events);
    assert_eq!(
        g.effect(hook_exec).unwrap().attribution,
        Attribution::Infrastructure
    );
    let fork_of_hook = g
        .effects
        .iter()
        .find(|e| e.kind == "effect.proc.fork" && e.ts_ns == 11)
        .unwrap();
    assert_eq!(fork_of_hook.attribution, Attribution::Infrastructure);
    assert_eq!(g.effect(w).unwrap().call_id.as_deref(), Some("c"));
    assert!(!g.call("c").unwrap().effects.contains(&hook_exec));
}
