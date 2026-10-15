//! Synthetic sessions with known ground truth, for the attribution eval
//! and the native-vs-Wasm parity check (feature `synth`).
#![allow(
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::range_plus_one,
    clippy::missing_panics_doc
)]

use std::collections::BTreeMap;

use wyrd_schema::{
    CallStatus, Event, EventBody, FileOpenWrite, FilePath, IntentSource, ProcExec, ProcFork,
    ProcessRef, ToolCallFinished, ToolCallStarted,
};

/// Small deterministic RNG (xorshift64*).
#[derive(Debug)]
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// Ground truth of one labelled effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Truth {
    Call(String),
    None,
    Ambiguous,
}

/// A generated session with ground truth per effect sequence number.
#[derive(Debug)]
pub struct Gen {
    pub events: Vec<Event>,
    pub truth: BTreeMap<u64, Truth>,
    t: u64,
    next_pid: u32,
}

const ROOT: (u32, u64) = (1000, 1);

impl Gen {
    fn ev(&mut self, ts: u64, p: Option<(u32, u64, u32)>, body: EventBody, truth: Option<Truth>) {
        let seq = self.events.len() as u64 + 1;
        self.events.push(Event {
            seq,
            ts_mono_ns: ts,
            ts_wall_ms: 0,
            process: p.map(|(pid, start, ppid)| ProcessRef {
                pid,
                tid: pid,
                ppid,
                start_ns: start,
                cgroup_id: 1,
            }),
            drops_since_last: 0,
            body,
        });
        if let Some(t) = truth {
            self.truth.insert(seq, t);
        }
    }
    fn tick(&mut self, by: u64) -> u64 {
        self.t += by;
        self.t
    }
    fn pid(&mut self) -> (u32, u64) {
        self.next_pid += 1;
        // Occasionally reuse a pid number with a new start time.
        (1000 + (self.next_pid % 40), self.t)
    }
    fn write(&mut self, p: (u32, u64), ppid: u32, path: String, truth: Truth) {
        let ts = self.tick(3);
        self.ev(
            ts,
            Some((p.0, p.1, ppid)),
            EventBody::FileOpenWrite(FileOpenWrite {
                file: FilePath {
                    path,
                    in_cell: true,
                    truncated: false,
                },
                flags: 577,
            }),
            Some(truth),
        );
    }
    fn fork_exec(
        &mut self,
        parent: (u32, u64),
        ppid_of_parent: u32,
        argv: Vec<String>,
        truth: Truth,
    ) -> (u32, u64) {
        let child = self.pid();
        let ts = self.tick(2);
        self.ev(
            ts,
            Some((parent.0, parent.1, ppid_of_parent)),
            EventBody::ProcFork(ProcFork {
                child_pid: child.0,
                child_start_ns: child.1,
            }),
            None,
        );
        let ts = self.tick(2);
        self.ev(
            ts,
            Some((child.0, child.1, parent.0)),
            EventBody::ProcExec(ProcExec {
                filename: format!("/usr/bin/{}", argv[0]),
                argv,
                cwd: Some("/w".into()),
                truncated: false,
            }),
            Some(truth),
        );
        child
    }
    fn start(&mut self, id: &str, tool: &str, args: serde_json::Value, source: IntentSource) {
        let ts = self.tick(5);
        self.ev(
            ts,
            None,
            EventBody::ToolCallStarted(ToolCallStarted {
                call_id: id.into(),
                source,
                tool: tool.into(),
                args,
                turn: Some(1),
            }),
            None,
        );
    }
    fn finish(&mut self, id: &str) {
        let ts = self.tick(5);
        self.ev(
            ts,
            None,
            EventBody::ToolCallFinished(ToolCallFinished {
                call_id: id.into(),
                status: CallStatus::Ok,
                exit_code: Some(0),
                summary: None,
                duration_ms: 1,
            }),
            None,
        );
    }
}

/// Generate session number `seed` (deterministic).
pub fn session(seed: u64) -> Gen {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut g = Gen {
        events: Vec::new(),
        truth: BTreeMap::new(),
        t: 10,
        next_pid: 0,
    };
    g.ev(
        10,
        Some((ROOT.0, ROOT.1, 1)),
        EventBody::ProcExec(ProcExec {
            filename: "/usr/bin/agent".into(),
            argv: vec!["agent".into()],
            cwd: Some("/w".into()),
            truncated: false,
        }),
        None,
    );
    // A long-lived MCP server started by the agent.
    let server = g.fork_exec(
        ROOT,
        1,
        vec!["mcp-server".into(), "--stdio".into()],
        Truth::None,
    );
    g.truth.remove(&(g.events.len() as u64));
    let mut background: Vec<((u32, u64), u32, String)> = Vec::new();
    let calls = 3 + r.below(10);
    for c in 0..calls {
        let id = format!("s{seed}c{c}");
        match r.below(5) {
            // Bash with a subtree; maybe a background process that outlives the call.
            0 | 1 => {
                let words = [
                    "pnpm test",
                    "cargo build --release --all-features",
                    "make -j8 check",
                    "npm run lint -- --fix",
                    "pytest -q tests/unit/test_cart.py",
                ];
                let cmd = words[r.below(words.len() as u64) as usize].to_string();
                g.start(
                    &id,
                    "Bash",
                    serde_json::json!({ "command": cmd }),
                    IntentSource::Hook,
                );
                // Half the time the agent wraps the command the way Claude
                // Code's Bash tool does, so the 63 bytes the kernel keeps never
                // contain it; otherwise `sh -c <command>` (also cut at 63 bytes).
                let argv: Vec<String> = if r.chance(50) {
                    let wrapped = format!(
                        "source /home/u/.claude/shell-snapshots/snapshot-bash-1792073200465-ubd2jm.sh 2>/dev/null || true && eval '{cmd}' < /dev/null"
                    );
                    vec![
                        "/bin/bash".into(),
                        "-c".into(),
                        "-l".into(),
                        wrapped.chars().take(63).collect(),
                    ]
                } else {
                    vec!["sh".into(), "-c".into(), cmd.chars().take(63).collect()]
                };
                let sh = g.fork_exec(ROOT, 1, argv, Truth::Call(id.clone()));
                for k in 0..(1 + r.below(3)) {
                    let w = g.fork_exec(
                        sh,
                        ROOT.0,
                        vec![format!("tool{k}")],
                        Truth::Call(id.clone()),
                    );
                    for f in 0..(1 + r.below(4)) {
                        g.write(
                            w,
                            sh.0,
                            format!("/w/out/{id}-{k}-{f}"),
                            Truth::Call(id.clone()),
                        );
                    }
                    if r.chance(20) {
                        background.push((w, sh.0, id.clone()));
                    }
                }
                g.finish(&id);
            }
            // In-process edit (target + incidental agent writes during the call).
            2 => {
                let target = format!("/w/src/{id}.rs");
                g.start(
                    &id,
                    "Edit",
                    serde_json::json!({ "file_path": target, "old_string": "a" }),
                    IntentSource::Hook,
                );
                g.write(ROOT, 1, target, Truth::Call(id.clone()));
                if r.chance(50) {
                    g.write(
                        ROOT,
                        1,
                        format!("/w/src/{id}.rs.tmp"),
                        Truth::Call(id.clone()),
                    );
                }
                g.finish(&id);
            }
            // MCP tool: hook call + nested gateway call; the server does the work.
            3 => {
                g.start(
                    &id,
                    "mcp__fs__write_file",
                    serde_json::json!({ "path": "/w/data" }),
                    IntentSource::Hook,
                );
                let gw = format!("{id}g");
                g.start(&gw, "write_file", serde_json::json!({}), IntentSource::Mcp);
                for f in 0..(1 + r.below(3)) {
                    g.write(
                        server,
                        ROOT.0,
                        format!("/w/data/{id}-{f}"),
                        Truth::Call(id.clone()),
                    );
                }
                g.finish(&gw);
                g.finish(&id);
            }
            // Two overlapping calls: each has a direct child, plus agent writes in the overlap.
            _ => {
                let a = format!("{id}a");
                let b = format!("{id}b");
                g.start(
                    &a,
                    "Bash",
                    serde_json::json!({ "command": "sleep 1 && echo a" }),
                    IntentSource::Hook,
                );
                g.start(
                    &b,
                    "Bash",
                    serde_json::json!({ "command": "echo b > b.txt" }),
                    IntentSource::Hook,
                );
                let pa = g.fork_exec(
                    ROOT,
                    1,
                    vec!["sh".into(), "-c".into(), "sleep 1 && echo a".into()],
                    Truth::Call(a.clone()),
                );
                let pb = g.fork_exec(
                    ROOT,
                    1,
                    vec!["sh".into(), "-c".into(), "echo b > b.txt".into()],
                    Truth::Call(b.clone()),
                );
                g.write(pb, ROOT.0, format!("/w/{b}.txt"), Truth::Call(b.clone()));
                g.write(pa, ROOT.0, format!("/w/{a}.log"), Truth::Call(a.clone()));
                g.write(ROOT, 1, format!("/w/.agent-{id}"), Truth::Ambiguous);
                g.finish(&b);
                g.finish(&a);
            }
        }
        // Gaps: agent runtime noise and unrelated processes.
        if r.chance(40) {
            g.write(ROOT, 1, "/home/u/.agent/state.json".into(), Truth::None);
        }
        if r.chance(25) {
            let stranger = (5000 + c as u32, g.t);
            g.write(stranger, 1, format!("/w/stranger-{id}"), Truth::None);
        }
        // Background processes keep writing later.
        if let Some((p, ppid, owner)) = background.pop() {
            g.write(p, ppid, format!("/w/out/bg-{owner}"), Truth::Call(owner));
        }
    }
    g
}
