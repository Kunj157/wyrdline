//! Causal correlation of a session: which tool call caused which kernel
//! effect.
//!
//! [`correlate`] is a pure function of the event list (no I/O, clocks, or
//! threads) so the same code runs in the daemon and, compiled to
//! WebAssembly, in the browser. The output is deterministic.
//!
//! Attribution, strongest first (plan §17.1):
//!
//! 1. **Direct** (confidence 1.0): a command tool's exec matched by its
//!    command line, and every later process in that subtree; an edit or
//!    write tool's file effects on the target path inside its window.
//! 2. **Window** (0.8): a process the agent forks while exactly one command
//!    call is open, when that call's exec cannot be matched (wrapped or
//!    truncated commands), owns its whole subtree; any other effect of the
//!    agent's process tree inside exactly one open call window. Inside
//!    several windows the effect is ambiguous (0.5 / n, candidates listed,
//!    none picked).
//! 3. **Unattributed**: outside every window — by the agent process itself
//!    (`agent_runtime`, e.g. its own state files) or by anything else.
//!
//! Wyrdline's own hook helpers (`wyrd hook …`), which the agent spawns
//! around every tool call, are `infrastructure` and never part of a call.

#[cfg(feature = "synth")]
pub mod synth;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wyrd_schema::{CallStatus, Event, EventBody, IntentSource, Plane};

/// Confidence in basis points.
pub const DIRECT: u16 = 10_000;
pub const WINDOW: u16 = 8_000;
pub const AMBIGUOUS: u16 = 5_000;

/// Process identity: pid plus start time (PID-reuse safe).
pub type ProcKey = (u32, u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    Direct,
    Window,
    Ambiguous,
    AgentRuntime,
    /// Wyrdline's own helper processes (hook adapters) that the agent runs.
    Infrastructure,
    Unattributed,
}

/// One tool call with its time window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
pub struct Call {
    pub call_id: String,
    pub tool: String,
    pub source: IntentSource,
    pub started_seq: u64,
    pub finished_seq: Option<u64>,
    pub start_ns: u64,
    /// End of the window; open calls extend to the end of the session.
    pub end_ns: u64,
    pub status: Option<CallStatus>,
    pub exit_code: Option<i32>,
    /// Shell command for command tools.
    pub command: Option<String>,
    /// Target file for edit/write tools.
    pub target_path: Option<String>,
    /// MCP calls that are the gateway's view of a hook call carry the hook
    /// call's id here; they never compete with it.
    pub nested_in: Option<String>,
    /// Effects attributed to this call (sequence numbers).
    pub effects: Vec<u64>,
}

/// One process of the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
pub struct Process {
    pub pid: u32,
    pub start_ns: u64,
    pub ppid: u32,
    pub exe: Option<String>,
    pub argv: Vec<String>,
    pub exec_seq: Option<u64>,
    pub exit_code: Option<i32>,
    /// Call that started this process (or an ancestor), if known.
    pub call_id: Option<String>,
    pub attribution: Attribution,
}

/// One effect and its cause.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
pub struct Effect {
    pub seq: u64,
    pub kind: String,
    pub ts_ns: u64,
    pub pid: u32,
    /// The attributed call, when there is exactly one.
    pub call_id: Option<String>,
    /// For ambiguous effects: every call whose window contains it.
    pub candidates: Vec<String>,
    pub attribution: Attribution,
    pub confidence_bp: u16,
}

/// The causal graph of a session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
pub struct Graph {
    pub calls: Vec<Call>,
    pub processes: Vec<Process>,
    pub effects: Vec<Effect>,
    /// The agent's root process (first exec of the session), if seen.
    pub agent_root: Option<u32>,
}

impl Graph {
    pub fn call(&self, id: &str) -> Option<&Call> {
        self.calls.iter().find(|c| c.call_id == id)
    }

    pub fn effect(&self, seq: u64) -> Option<&Effect> {
        self.effects
            .binary_search_by_key(&seq, |e| e.seq)
            .ok()
            .map(|i| &self.effects[i])
    }

    /// Counts per attribution kind.
    pub fn summary(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for e in &self.effects {
            *m.entry(format!("{:?}", e.attribution).to_lowercase())
                .or_insert(0) += 1;
        }
        m
    }
}

const COMMAND_TOOLS: &[&str] = &[
    "bash",
    "shell",
    "run_shell_command",
    "exec_command",
    "local_shell",
    "terminal",
];
const FILE_TOOLS: &[&str] = &[
    "edit",
    "multiedit",
    "write",
    "notebookedit",
    "write_file",
    "replace",
    "apply_patch",
];

fn is_tool(tool: &str, names: &[&str]) -> bool {
    let t = tool.to_ascii_lowercase();
    names
        .iter()
        .any(|n| t == *n || t.ends_with(&format!("__{n}")))
}

fn arg_str(args: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        args.get(*k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

/// Whitespace-normalised.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does exec argv run `command`? Matches `sh -c <command>` (any shell, any
/// flags before `-c`) and direct execs. The kernel keeps 64 bytes per
/// argument, so a truncated argument matches as a prefix.
fn exec_runs(argv: &[String], command: &str) -> bool {
    let want = norm(command);
    if want.is_empty() {
        return false;
    }
    let prefix_eq = |got: &str| {
        let got = norm(got);
        !got.is_empty() && (got == want || (got.len() >= 48 && want.starts_with(&got)))
    };
    if let Some(i) = argv
        .iter()
        .position(|a| a == "-c" || (a.starts_with('-') && a.ends_with('c') && a.len() <= 4))
        && let Some(script) = argv.get(i + 1)
    {
        return prefix_eq(script);
    }
    prefix_eq(&argv.join(" "))
}

/// Wyrdline helpers the agent itself spawns: hook adapters.
fn is_infrastructure(p: &Process) -> bool {
    let name = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    let exe = p
        .exe
        .as_deref()
        .map(name)
        .or_else(|| p.argv.first().map(|a| name(a)));
    exe.as_deref() == Some("wyrd") && p.argv.get(1).is_some_and(|a| a == "hook")
}

fn path_matches(effect_path: &str, target: &str) -> bool {
    effect_path == target
        || (!target.starts_with('/') && effect_path.ends_with(&format!("/{target}")))
}

/// Path(s) an effect touches, for matching file tools.
fn effect_paths(body: &EventBody) -> Vec<&str> {
    match body {
        EventBody::FileOpenWrite(x) => vec![&x.file.path],
        EventBody::FileWrite(x) => vec![&x.file.path],
        EventBody::FileRename(x) => vec![&x.from.path, &x.to.path],
        EventBody::FileUnlink(x) => vec![&x.file.path],
        EventBody::FileMkdir(x) | EventBody::FileReadSensitive(x) => vec![&x.path],
        EventBody::FileChmod(x) => vec![&x.file.path],
        EventBody::FileTruncate(x) => vec![&x.file.path],
        _ => Vec::new(),
    }
}

/// Correlate one session.
#[allow(clippy::too_many_lines)]
pub fn correlate(events: &[Event]) -> Graph {
    let session_end = events.iter().map(|e| e.ts_mono_ns).max().unwrap_or(0);

    // ---- calls
    let mut calls: Vec<Call> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for e in events {
        match &e.body {
            EventBody::ToolCallStarted(s) => {
                if index.contains_key(&s.call_id) {
                    continue;
                }
                let command = is_tool(&s.tool, COMMAND_TOOLS)
                    .then(|| arg_str(&s.args, &["command", "cmd"]))
                    .flatten();
                let target_path = is_tool(&s.tool, FILE_TOOLS)
                    .then(|| arg_str(&s.args, &["file_path", "path", "notebook_path", "filename"]))
                    .flatten();
                index.insert(s.call_id.clone(), calls.len());
                calls.push(Call {
                    call_id: s.call_id.clone(),
                    tool: s.tool.clone(),
                    source: s.source,
                    started_seq: e.seq,
                    finished_seq: None,
                    start_ns: e.ts_mono_ns,
                    end_ns: session_end,
                    status: None,
                    exit_code: None,
                    command,
                    target_path,
                    nested_in: None,
                    effects: Vec::new(),
                });
            }
            EventBody::ToolCallFinished(f) => {
                if let Some(&i) = index.get(&f.call_id) {
                    let c = &mut calls[i];
                    c.finished_seq = Some(e.seq);
                    c.end_ns = e.ts_mono_ns;
                    c.status = Some(f.status);
                    c.exit_code = f.exit_code;
                }
            }
            EventBody::ToolCallDenied(d) => {
                if let Some(&i) = index.get(&d.call_id) {
                    calls[i].finished_seq = Some(e.seq);
                    calls[i].end_ns = e.ts_mono_ns;
                    calls[i].status = Some(CallStatus::Denied);
                }
            }
            _ => {}
        }
    }
    // Gateway (MCP) calls inside a hook call of an MCP tool are the same call:
    // attach to the smallest enclosing one (ties: the earliest started).
    for i in 0..calls.len() {
        if calls[i].source != IntentSource::Mcp {
            continue;
        }
        let parent = calls
            .iter()
            .filter(|p| p.source != IntentSource::Mcp && p.tool.starts_with("mcp__"))
            .filter(|p| p.start_ns <= calls[i].start_ns && calls[i].end_ns <= p.end_ns)
            .min_by_key(|p| (p.end_ns - p.start_ns, p.start_ns))
            .map(|p| p.call_id.clone());
        calls[i].nested_in = parent;
    }

    // ---- processes
    let mut procs: BTreeMap<ProcKey, Process> = BTreeMap::new();
    // pid → current key (latest start time seen)
    let mut current: BTreeMap<u32, ProcKey> = BTreeMap::new();
    let mut agent_root: Option<ProcKey> = None;
    let mut children: BTreeMap<ProcKey, Vec<ProcKey>> = BTreeMap::new();
    let mut forked_at: BTreeMap<ProcKey, u64> = BTreeMap::new();
    let key_of = |p: &wyrd_schema::ProcessRef| (p.pid, p.start_ns);
    for e in events.iter().filter(|e| e.plane() == Plane::Effect) {
        let Some(p) = e.process else { continue };
        let k = key_of(&p);
        let entry = procs.entry(k).or_insert_with(|| Process {
            pid: p.pid,
            start_ns: p.start_ns,
            ppid: p.ppid,
            exe: None,
            argv: Vec::new(),
            exec_seq: None,
            exit_code: None,
            call_id: None,
            attribution: Attribution::Unattributed,
        });
        current.insert(p.pid, k);
        match &e.body {
            EventBody::ProcExec(x) => {
                entry.exe = Some(x.filename.clone());
                entry.argv.clone_from(&x.argv);
                entry.exec_seq.get_or_insert(e.seq);
                if agent_root.is_none() {
                    agent_root = Some(k);
                }
            }
            EventBody::ProcExit(x) => entry.exit_code = Some(x.exit_code),
            EventBody::ProcFork(f) => {
                let child = (f.child_pid, f.child_start_ns);
                procs.entry(child).or_insert_with(|| Process {
                    pid: f.child_pid,
                    start_ns: f.child_start_ns,
                    ppid: p.pid,
                    exe: None,
                    argv: Vec::new(),
                    exec_seq: None,
                    exit_code: None,
                    call_id: None,
                    attribution: Attribution::Unattributed,
                });
                current.insert(f.child_pid, child);
                children.entry(k).or_default().push(child);
                forked_at.insert(child, e.ts_mono_ns);
            }
            _ => {}
        }
    }
    // Parent links for processes first seen without a fork event.
    let parent_of: BTreeMap<ProcKey, ProcKey> = {
        let mut m = BTreeMap::new();
        for (parent, kids) in &children {
            for kid in kids {
                m.insert(*kid, *parent);
            }
        }
        for (k, p) in &procs {
            if !m.contains_key(k)
                && let Some(pk) = current.get(&p.ppid).copied().filter(|pk| pk != k)
            {
                m.insert(*k, pk);
            }
        }
        m
    };
    let in_agent_tree = |mut k: ProcKey| -> bool {
        let Some(root) = agent_root else { return true };
        for _ in 0..256 {
            if k == root {
                return true;
            }
            match parent_of.get(&k) {
                Some(p) => k = *p,
                None => return false,
            }
        }
        false
    };

    // ---- Wyrdline's own helpers and their subtrees.
    let infra: BTreeSet<ProcKey> = {
        let roots: BTreeSet<ProcKey> = procs
            .iter()
            .filter(|(_, p)| is_infrastructure(p))
            .map(|(k, _)| *k)
            .collect();
        procs
            .keys()
            .filter(|k| {
                let mut cur = **k;
                for _ in 0..256 {
                    if roots.contains(&cur) {
                        return true;
                    }
                    match parent_of.get(&cur) {
                        Some(p) => cur = *p,
                        None => return false,
                    }
                }
                false
            })
            .copied()
            .collect()
    };

    // ---- direct process attribution: exec matched to a command call.
    let mut direct_root: BTreeMap<ProcKey, String> = BTreeMap::new();
    let mut used: BTreeSet<ProcKey> = BTreeSet::new();
    for c in &calls {
        let Some(cmd) = &c.command else { continue };
        let found = procs
            .iter()
            .filter(|(k, p)| {
                !used.contains(*k)
                    && p.exec_seq.is_some()
                    && events_ts(events, p.exec_seq)
                        .is_some_and(|t| c.start_ns <= t && t <= c.end_ns)
                    && exec_runs(&p.argv, cmd)
            })
            .min_by_key(|(_, p)| p.exec_seq)
            .map(|(k, _)| *k);
        if let Some(k) = found {
            used.insert(k);
            direct_root.insert(k, c.call_id.clone());
        }
    }
    // Window ownership: a command call whose exec could not be matched (the
    // agent wraps commands, e.g. `bash -c -l "source …; eval '…'"`, and the
    // kernel keeps only the first bytes of each argument) owns the processes
    // the agent forks while it is the only open command call.
    let command_windows: Vec<&Call> = calls
        .iter()
        .filter(|c| c.command.is_some() && c.nested_in.is_none())
        .collect();
    let matched: BTreeSet<&str> = direct_root.values().map(String::as_str).collect();
    let mut window_root: BTreeMap<ProcKey, String> = BTreeMap::new();
    if let Some(root) = agent_root {
        for (k, at) in &forked_at {
            if used.contains(k) || infra.contains(k) || parent_of.get(k) != Some(&root) {
                continue;
            }
            let open: Vec<&&Call> = command_windows
                .iter()
                .filter(|c| c.start_ns <= *at && *at <= c.end_ns)
                .collect();
            if let [only] = open.as_slice()
                && !matched.contains(only.call_id.as_str())
            {
                window_root.insert(*k, only.call_id.clone());
            }
        }
    }
    // Propagate to descendants (background processes stay attributed).
    let owner_of = |mut k: ProcKey| -> Option<(String, Attribution)> {
        for _ in 0..256 {
            if let Some(c) = direct_root.get(&k) {
                return Some((c.clone(), Attribution::Direct));
            }
            if let Some(c) = window_root.get(&k) {
                return Some((c.clone(), Attribution::Window));
            }
            k = *parent_of.get(&k)?;
        }
        None
    };
    for (k, p) in &mut procs {
        if infra.contains(k) {
            p.attribution = Attribution::Infrastructure;
            continue;
        }
        if let Some((c, how)) = owner_of(*k) {
            p.call_id = Some(c);
            p.attribution = how;
        }
    }

    // ---- effects
    let competing: Vec<usize> = (0..calls.len())
        .filter(|i| calls[*i].nested_in.is_none())
        .collect();
    let mut effects = Vec::new();
    for e in events.iter().filter(|e| e.plane() == Plane::Effect) {
        let Some(p) = e.process else { continue };
        let k = key_of(&p);
        let t = e.ts_mono_ns;
        let mut eff = Effect {
            seq: e.seq,
            kind: e.kind().to_string(),
            ts_ns: t,
            pid: p.pid,
            call_id: None,
            candidates: Vec::new(),
            attribution: Attribution::Unattributed,
            confidence_bp: 0,
        };
        let creates_infra = matches!(&e.body, EventBody::ProcFork(f) if infra.contains(&(f.child_pid, f.child_start_ns)));
        if infra.contains(&k) || creates_infra {
            eff.attribution = Attribution::Infrastructure;
        } else if let Some(pr) = procs.get(&k).filter(|pr| pr.call_id.is_some()) {
            eff.call_id.clone_from(&pr.call_id);
            eff.attribution = pr.attribution;
            eff.confidence_bp = if pr.attribution == Attribution::Direct {
                DIRECT
            } else {
                WINDOW
            };
        } else {
            let open: Vec<&Call> = competing
                .iter()
                .map(|i| &calls[*i])
                .filter(|c| c.start_ns <= t && t <= c.end_ns)
                .collect();
            let paths = effect_paths(&e.body);
            let file_hit = open.iter().find(|c| {
                c.target_path
                    .as_deref()
                    .is_some_and(|tp| paths.iter().any(|ep| path_matches(ep, tp)))
            });
            if let Some(c) = file_hit {
                eff.call_id = Some(c.call_id.clone());
                eff.attribution = Attribution::Direct;
                eff.confidence_bp = DIRECT;
            } else if in_agent_tree(k) && open.len() == 1 {
                eff.call_id = Some(open[0].call_id.clone());
                eff.attribution = Attribution::Window;
                eff.confidence_bp = WINDOW;
            } else if in_agent_tree(k) && open.len() > 1 {
                eff.candidates = open.iter().map(|c| c.call_id.clone()).collect();
                eff.attribution = Attribution::Ambiguous;
                eff.confidence_bp =
                    AMBIGUOUS / u16::try_from(open.len()).unwrap_or(u16::MAX).max(1);
            } else if Some(k) == agent_root {
                eff.attribution = Attribution::AgentRuntime;
            }
        }
        if let Some(c) = &eff.call_id
            && let Some(&i) = index.get(c)
        {
            calls[i].effects.push(e.seq);
        }
        effects.push(eff);
    }

    Graph {
        calls,
        processes: procs.into_values().collect(),
        effects,
        agent_root: agent_root.map(|k| k.0),
    }
}

fn events_ts(events: &[Event], seq: Option<u64>) -> Option<u64> {
    let seq = seq?;
    events
        .binary_search_by_key(&seq, |e| e.seq)
        .ok()
        .map(|i| events[i].ts_mono_ns)
}
