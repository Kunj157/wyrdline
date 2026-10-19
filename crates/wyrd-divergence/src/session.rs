//! Facts about one session that the rules share: writes, runs, process
//! ancestry, connects, transcript text — derived once from the events and
//! the correlator graph.

use std::collections::{BTreeMap, HashMap};

use wyrd_correlator::{Attribution, Graph};
use wyrd_schema::{CallStatus, Event, EventBody, NetVerdict, Role};

/// A file-changing effect.
#[derive(Debug, Clone)]
pub struct Write {
    pub seq: u64,
    pub ts: u64,
    pub pid: Option<u32>,
    /// Absolute path as observed.
    pub path: String,
    /// Workspace-relative path, `None` outside the workspace.
    pub rel: Option<String>,
    pub unlink: bool,
    pub call_id: Option<String>,
    pub attribution: Option<Attribution>,
}

/// A command that ran: a kernel exec, or (sessions without kernel
/// effects) a command tool call.
#[derive(Debug, Clone)]
pub struct Run {
    pub seq: u64,
    pub ts: u64,
    pub pid: Option<u32>,
    pub cmdline: String,
    pub exit: Option<i32>,
    pub call_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Connect {
    pub seq: u64,
    pub ts: u64,
    pub pid: Option<u32>,
    pub host: String,
    pub addr: String,
    pub port: u16,
    pub verdict: NetVerdict,
    pub attribution: Option<Attribution>,
}

/// Index over a session.
#[derive(Debug)]
pub struct Session<'a> {
    pub events: &'a [Event],
    pub graph: &'a Graph,
    pub root: String,
    pub writes: Vec<Write>,
    pub runs: Vec<Run>,
    pub connects: Vec<Connect>,
    /// User messages, joined: the task as given.
    pub task: String,
    /// Every transcript message and every tool argument, lowercased (D007).
    pub mentioned: String,
    /// pid → parent pid (latest process with that pid wins).
    parents: HashMap<u32, u32>,
    ts: BTreeMap<u64, u64>,
}

impl<'a> Session<'a> {
    pub fn new(events: &'a [Event], graph: &'a Graph) -> Self {
        let root = events
            .iter()
            .find_map(|e| match &e.body {
                EventBody::SessionStarted(s) => {
                    Some(s.workspace_root.trim_end_matches('/').to_string())
                }
                _ => None,
            })
            .unwrap_or_default();
        let rel = |p: &str| -> Option<String> {
            if root.is_empty() {
                return (!p.starts_with('/')).then(|| p.to_string());
            }
            p.strip_prefix(&root)
                .and_then(|r| r.strip_prefix('/'))
                .map(str::to_string)
        };
        let effect = |seq: u64| graph.effect(seq);
        let mut writes = Vec::new();
        let mut connects = Vec::new();
        let mut task = String::new();
        let mut mentioned = String::new();
        let mut ts = BTreeMap::new();
        for e in events {
            ts.insert(e.seq, e.ts_mono_ns);
            let pid = e.process.map(|p| p.pid);
            let mut push = |path: &str, unlink: bool| {
                let fx = effect(e.seq);
                writes.push(Write {
                    seq: e.seq,
                    ts: e.ts_mono_ns,
                    pid,
                    path: path.to_string(),
                    rel: rel(path),
                    unlink,
                    call_id: fx.and_then(|f| f.call_id.clone()),
                    attribution: fx.map(|f| f.attribution),
                });
            };
            match &e.body {
                EventBody::FileOpenWrite(w) => push(&w.file.path, false),
                EventBody::FileWrite(w) => push(&w.file.path, false),
                EventBody::FileTruncate(w) => push(&w.file.path, false),
                EventBody::FileRename(r) => {
                    push(&r.from.path, true);
                    push(&r.to.path, false);
                }
                EventBody::FileUnlink(u) => push(&u.file.path, true),
                EventBody::NetConnect(c) => connects.push(Connect {
                    seq: e.seq,
                    ts: e.ts_mono_ns,
                    pid,
                    host: c.dns_name.clone().unwrap_or_else(|| c.addr.clone()),
                    addr: c.addr.clone(),
                    port: c.port,
                    verdict: c.verdict,
                    attribution: effect(e.seq).map(|f| f.attribution),
                }),
                EventBody::Message(m) => {
                    if m.role == Role::User {
                        task.push_str(&m.text);
                        task.push('\n');
                    }
                    mentioned.push_str(&m.text.to_lowercase());
                    mentioned.push('\n');
                }
                EventBody::ToolCallStarted(t) => {
                    mentioned.push_str(&t.args.to_string().to_lowercase());
                    mentioned.push('\n');
                }
                EventBody::McpRequest(r) => {
                    mentioned.push_str(&r.params.to_string().to_lowercase());
                    mentioned.push('\n');
                }
                _ => {}
            }
        }
        let has_execs = events
            .iter()
            .any(|e| matches!(e.body, EventBody::ProcExec(_)));
        let runs = if has_execs {
            kernel_runs(events, graph)
        } else {
            tool_runs(events, graph)
        };
        let parents = graph.processes.iter().map(|p| (p.pid, p.ppid)).collect();
        Self {
            events,
            graph,
            root,
            writes,
            runs,
            connects,
            task,
            mentioned,
            parents,
            ts,
        }
    }

    pub fn ts(&self, seq: u64) -> u64 {
        self.ts.get(&seq).copied().unwrap_or(0)
    }

    /// True when `pid` is `ancestor` or one of its descendants.
    pub fn descends_from(&self, mut pid: u32, ancestor: u32) -> bool {
        for _ in 0..256 {
            if pid == ancestor {
                return true;
            }
            match self.parents.get(&pid) {
                Some(&pp) if pp != pid && pp != 0 => pid = pp,
                _ => return false,
            }
        }
        false
    }

    /// True when `pid` belongs to the agent's process tree (or the tree
    /// is unknown).
    pub fn in_agent_tree(&self, pid: Option<u32>) -> bool {
        match (pid, self.graph.agent_root) {
            (Some(pid), Some(root)) => self.descends_from(pid, root),
            _ => true,
        }
    }

    pub fn event(&self, seq: u64) -> Option<&Event> {
        let i = self.events.binary_search_by_key(&seq, |e| e.seq).ok()?;
        self.events.get(i)
    }
}

fn kernel_runs(events: &[Event], graph: &Graph) -> Vec<Run> {
    let exits: HashMap<u64, Option<i32>> = graph
        .processes
        .iter()
        .filter_map(|p| p.exec_seq.map(|s| (s, p.exit_code)))
        .collect();
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ProcExec(x) => Some(Run {
                seq: e.seq,
                ts: e.ts_mono_ns,
                pid: e.process.map(|p| p.pid),
                cmdline: if x.argv.is_empty() {
                    x.filename.clone()
                } else {
                    x.argv.join(" ")
                },
                exit: exits.get(&e.seq).copied().flatten(),
                call_id: graph.effect(e.seq).and_then(|f| f.call_id.clone()),
            }),
            _ => None,
        })
        .collect()
}

/// Without kernel effects the command tool calls are the only evidence
/// of what ran (lower confidence: the agent reported them).
fn tool_runs(events: &[Event], graph: &Graph) -> Vec<Run> {
    graph
        .calls
        .iter()
        // A denied or unfinished call never ran (as far as anyone reported).
        .filter(|c| matches!(c.status, Some(CallStatus::Ok | CallStatus::Error)))
        .filter_map(|c| {
            let cmd = c.command.clone()?;
            let ts = events
                .iter()
                .find(|e| e.seq == c.started_seq)
                .map_or(c.start_ns, |e| e.ts_mono_ns);
            Some(Run {
                seq: c.started_seq,
                ts,
                pid: None,
                cmdline: cmd,
                exit: c.exit_code,
                call_id: Some(c.call_id.clone()),
            })
        })
        .collect()
}
