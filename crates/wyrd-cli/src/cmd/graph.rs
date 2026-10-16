//! `wyrd graph` and `wyrd explain`: the correlator's view of a session.

use std::io::Write as _;

use anyhow::Context as _;
use wyrd_cell::{DataDir, Session};
use wyrd_correlator::{Attribution, Graph};
use wyrd_schema::{Event, EventBody};

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct GraphArgs {
    /// Session id or unique prefix.
    session: String,
    /// Print the whole graph as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct ExplainArgs {
    session: String,
    /// Ledger sequence number of the effect.
    seq: u64,
    /// Print the explanation as JSON.
    #[arg(long)]
    json: bool,
}

fn load(session: &str) -> anyhow::Result<Vec<Event>> {
    let s = Session::open(&DataDir::from_env(), session)?;
    let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
    let mut all = Vec::new();
    loop {
        let batch = ledger.events_after(all.last().map_or(0, |e: &Event| e.seq), 5_000)?;
        if batch.is_empty() {
            break;
        }
        all.extend(batch);
    }
    Ok(all)
}

fn describe(body: &EventBody) -> String {
    match body {
        EventBody::ProcExec(x) => format!("exec {}", x.argv.join(" ")),
        EventBody::ProcExit(x) => format!("exit {}", x.exit_code),
        EventBody::ProcFork(x) => format!("fork → {}", x.child_pid),
        EventBody::FileOpenWrite(x) => format!("write {}", x.file.path),
        EventBody::FileWrite(x) => format!("write {}", x.file.path),
        EventBody::FileRename(x) => format!("rename {} → {}", x.from.path, x.to.path),
        EventBody::FileUnlink(x) => format!("delete {}", x.file.path),
        EventBody::FileMkdir(x) => format!("mkdir {}", x.path),
        EventBody::FileChmod(x) => format!("chmod {:o} {}", x.mode, x.file.path),
        EventBody::FileTruncate(x) => format!("truncate {}", x.file.path),
        EventBody::FileReadSensitive(x) => format!("read secret {}", x.path),
        EventBody::NetConnect(x) => format!("connect {}:{}", x.addr, x.port),
        EventBody::DnsQuery(x) => format!("dns {}", x.name),
        other => other.kind().to_string(),
    }
}

fn label(a: Attribution) -> &'static str {
    match a {
        Attribution::Direct => "direct",
        Attribution::Window => "window",
        Attribution::Ambiguous => "ambiguous",
        Attribution::AgentRuntime => "agent runtime",
        Attribution::Infrastructure => "wyrdline",
        Attribution::Unattributed => "unattributed",
    }
}

pub fn graph(a: &GraphArgs) -> anyhow::Result<u8> {
    let events = load(&a.session)?;
    let g: Graph = wyrd_correlator::correlate(&events);
    let mut o = std::io::stdout().lock();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &g)?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    let by_seq = |seq: u64| {
        events
            .binary_search_by_key(&seq, |e| e.seq)
            .ok()
            .map(|i| &events[i])
    };
    for c in g.calls.iter().filter(|c| c.nested_in.is_none()) {
        let what = c
            .command
            .as_deref()
            .or(c.target_path.as_deref())
            .unwrap_or("");
        let status = c
            .status
            .map_or("open".to_string(), |s| format!("{s:?}").to_lowercase());
        writeln!(
            o,
            "● {} {} {}  [{status}]  {} effect(s)",
            c.call_id,
            c.tool,
            what,
            c.effects.len()
        )?;
        for seq in &c.effects {
            if let (Some(ev), Some(eff)) = (by_seq(*seq), g.effect(*seq)) {
                writeln!(
                    o,
                    "    {:>6}  {:<9} {}",
                    seq,
                    label(eff.attribution),
                    describe(&ev.body)
                )?;
            }
        }
    }
    let loose: Vec<_> = g
        .effects
        .iter()
        .filter(|e| e.call_id.is_none() && e.attribution != Attribution::Infrastructure)
        .collect();
    if !loose.is_empty() {
        writeln!(o, "○ not caused by a single tool call: {}", loose.len())?;
        for eff in loose {
            if let Some(ev) = by_seq(eff.seq) {
                let extra = if eff.candidates.is_empty() {
                    String::new()
                } else {
                    format!(" (one of {})", eff.candidates.join(", "))
                };
                writeln!(
                    o,
                    "    {:>6}  {:<13} {}{extra}",
                    eff.seq,
                    label(eff.attribution),
                    describe(&ev.body)
                )?;
            }
        }
    }
    let summary: Vec<String> = g
        .summary()
        .into_iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect();
    writeln!(
        o,
        "{} call(s), {} effect(s): {}",
        g.calls.len(),
        g.effects.len(),
        summary.join(", ")
    )?;
    Ok(exit::OK)
}

pub fn explain(a: &ExplainArgs) -> anyhow::Result<u8> {
    let events = load(&a.session)?;
    let graph = wyrd_correlator::correlate(&events);
    let eff = graph
        .effect(a.seq)
        .with_context(|| format!("seq {} is not a kernel effect of this session", a.seq))?;
    let ev = events
        .iter()
        .find(|e| e.seq == a.seq)
        .context("event missing")?;
    let chain = process_chain(&graph, ev);
    let mut out = std::io::stdout().lock();
    if a.json {
        let call = eff.call_id.as_deref().and_then(|id| graph.call(id));
        let processes: Vec<_> = chain
            .iter()
            .map(|(pid, name)| serde_json::json!({ "pid": pid, "program": name }))
            .collect();
        let v = serde_json::json!({
            "effect": eff,
            "event": ev,
            "call": call,
            "process_chain": processes,
        });
        serde_json::to_writer_pretty(&mut out, &v)?;
        writeln!(out)?;
        return Ok(exit::OK);
    }
    writeln!(out, "effect   {} — {}", a.seq, describe(&ev.body))?;
    writeln!(
        out,
        "cause    {} ({:.0} % confidence)",
        label(eff.attribution),
        f64::from(eff.confidence_bp) / 100.0
    )?;
    match (&eff.call_id, eff.candidates.is_empty()) {
        (Some(id), _) => {
            if let Some(c) = graph.call(id) {
                writeln!(
                    out,
                    "call     {} {} {}",
                    c.call_id,
                    c.tool,
                    c.command
                        .as_deref()
                        .or(c.target_path.as_deref())
                        .unwrap_or("")
                )?;
            }
        }
        (None, false) => writeln!(out, "calls    one of {}", eff.candidates.join(", "))?,
        (None, true) => writeln!(out, "call     none")?,
    }
    write!(out, "process ")?;
    for (i, (pid, name)) in chain.iter().enumerate() {
        if i > 0 {
            write!(out, " ←")?;
        }
        write!(out, " {name}({pid})")?;
    }
    writeln!(out)?;
    Ok(exit::OK)
}

/// The effect's process and its ancestors up to the agent, as
/// (pid, program); at most 32 steps.
fn process_chain(graph: &Graph, ev: &Event) -> Vec<(u32, String)> {
    let mut chain = Vec::new();
    let mut pid = ev.process.map(|p| (p.pid, p.start_ns));
    for _ in 0..32 {
        let Some(k) = pid else { break };
        let p = graph.processes.iter().find(|p| (p.pid, p.start_ns) == k);
        let name = p
            .and_then(|p| p.argv.first().cloned())
            .unwrap_or_else(|| "?".into());
        chain.push((k.0, name));
        pid = p.and_then(|p| {
            graph
                .processes
                .iter()
                .filter(|q| q.pid == p.ppid)
                .map(|q| (q.pid, q.start_ns))
                .max_by_key(|k| k.1)
        });
        if pid == Some(k) {
            break;
        }
    }
    chain
}
