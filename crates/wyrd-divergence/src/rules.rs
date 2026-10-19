//! Rules D001–D010 (plan §17.4). Each is deterministic and cites the
//! ledger events that prove it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use wyrd_correlator::Attribution;
use wyrd_schema::{CallStatus, ClaimExtracted, ClaimType, EventBody, Severity};

use crate::claims::{Turn, claim_seq};
use crate::config::{Config, normalise};
use crate::session::{Session, Write};
use crate::{Finding, Input};

/// Policy id the MCP gateway uses for tool description pinning.
pub const PINNING_POLICY: &str = "wyrdline.mcp-pinning";

/// Most evidence seqs a single divergence cites.
const MAX_EVIDENCE: usize = 50;

pub(crate) fn all(input: &Input<'_>, s: &Session<'_>, c: &Config, turns: &[Turn]) -> Vec<Finding> {
    let mut out = Vec::new();
    d001(s, c, turns, input.claims, &mut out);
    d002(s, c, &mut out);
    d003(s, c, &mut out);
    d004(s, c, &mut out);
    d005(s, &mut out);
    d006(s, c, input, &mut out);
    d007(s, c, &mut out);
    d008(s, c, &mut out);
    d009(s, &mut out);
    d010(s, c, turns, input.claims, &mut out);
    out
}

fn finding(
    rule: &'static str,
    severity: Severity,
    claim: Option<&ClaimExtracted>,
    mut evidence: Vec<u64>,
    confidence_bp: u16,
    summary: String,
) -> Finding {
    evidence.sort_unstable();
    evidence.dedup();
    evidence.truncate(MAX_EVIDENCE);
    Finding {
        rule_id: rule,
        severity,
        claim_id: claim.map(|c| c.claim_id.clone()),
        evidence_seqs: evidence,
        confidence_bp,
        summary,
    }
}

fn is_loopback(addr: &str) -> bool {
    addr.parse::<std::net::IpAddr>().is_ok_and(|ip| match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    })
}

fn quote(c: &ClaimExtracted) -> String {
    let q: String = c.quote.chars().take(80).collect();
    if q.len() < c.quote.len() {
        format!("{q}…")
    } else {
        q
    }
}

/// Writes that count as the agent changing the project: inside the
/// workspace, outside build/cache dirs, not made by a test or build run
/// (their caches and reports), not by agent runtime or Wyrdline itself.
fn project_writes<'s>(s: &'s Session<'_>, c: &Config) -> Vec<&'s Write> {
    let runner_pids: Vec<u32> = s
        .runs
        .iter()
        .filter(|r| c.is_test_run(&r.cmdline) || c.is_build_run(&r.cmdline))
        .filter_map(|r| r.pid)
        .collect();
    s.writes
        .iter()
        .filter(|w| {
            w.rel
                .as_deref()
                .is_some_and(|r| !r.is_empty() && !c.in_build_dir(r))
                && !matches!(
                    w.attribution,
                    Some(Attribution::Infrastructure | Attribution::AgentRuntime)
                )
                && !w
                    .pid
                    .is_some_and(|p| runner_pids.iter().any(|&r| s.descends_from(p, r)))
        })
        .collect()
}

// ---------------------------------------------------------------- D001

fn d001(
    s: &Session<'_>,
    c: &Config,
    turns: &[Turn],
    claims: &[ClaimExtracted],
    out: &mut Vec<Finding>,
) {
    let writes = project_writes(s, c);
    for claim in claims {
        let Some(cseq) = claim_seq(turns, claim) else {
            continue;
        };
        let cts = s.ts(cseq);
        match claim.claim_type {
            ClaimType::RanCommand => {
                let ran = s
                    .runs
                    .iter()
                    .any(|r| r.ts <= cts && command_matches(&claim.subject, &r.cmdline));
                if !ran {
                    out.push(finding(
                        "D001",
                        Severity::High,
                        Some(claim),
                        vec![cseq],
                        9000,
                        format!(
                            "claimed \"{}\" but no `{}` ran before the claim",
                            quote(claim),
                            claim.subject
                        ),
                    ));
                }
            }
            ClaimType::TestsPass | ClaimType::BuildOk => {
                let tests = claim.claim_type == ClaimType::TestsPass;
                let what = if tests { "test" } else { "build" };
                let last = writes.iter().filter(|w| w.ts < cts).max_by_key(|w| w.ts);
                let after = last.map_or(0, |w| w.ts);
                let candidates: Vec<_> = s
                    .runs
                    .iter()
                    .filter(|r| r.ts > after && r.ts <= cts)
                    .filter(|r| c.is_test_run(&r.cmdline) || (!tests && c.is_build_run(&r.cmdline)))
                    .collect();
                if candidates.iter().any(|r| r.exit == Some(0)) {
                    continue;
                }
                let mut evidence = vec![cseq];
                evidence.extend(last.map(|w| w.seq));
                evidence.extend(candidates.iter().map(|r| r.seq));
                let failing: Vec<String> = candidates
                    .iter()
                    .filter_map(|r| r.exit.map(|e| format!("seq {} exited {e}", r.seq)))
                    .collect();
                let detail = match (last, failing.is_empty()) {
                    (Some(w), true) => format!(
                        "no {what} run exited 0 after the last write (seq {}, {})",
                        w.seq,
                        w.rel.as_deref().unwrap_or(&w.path)
                    ),
                    (None, true) => format!("no {what} run exited 0 before the claim"),
                    (_, false) => format!("{what} runs failed: {}", failing.join(", ")),
                };
                out.push(finding(
                    "D001",
                    Severity::High,
                    Some(claim),
                    evidence,
                    if failing.is_empty() { 9500 } else { 9000 },
                    format!("claimed \"{}\" but {detail}", quote(claim)),
                ));
            }
            _ => {}
        }
    }
}

/// The claimed command's program (and first argument, if any) appear in
/// one shell segment of the run.
fn command_matches(claimed: &str, cmdline: &str) -> bool {
    let want = normalise(claimed);
    let have = normalise(cmdline);
    want.iter().any(|w| {
        let mut wt = w.split_whitespace();
        let prog = wt.next().unwrap_or_default();
        let arg = wt.find(|t| !t.starts_with('-'));
        have.iter().any(|h| {
            let mut ht = h.split_whitespace();
            ht.next() == Some(prog) && arg.is_none_or(|a| h.split_whitespace().any(|t| t == a))
        })
    })
}

// ---------------------------------------------------------------- D002

static PATHISH: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(r"[\w.-]*[/.][\w./-]*\w").expect("constant pattern")
});

fn d002(s: &Session<'_>, c: &Config, out: &mut Vec<Finding>) {
    let task = s.task.to_lowercase();
    // Protected paths the prompt itself names are in scope.
    let named = |rel: &str| {
        let rel = rel.to_lowercase();
        PATHISH.find_iter(&task).any(|m| {
            let t = m.as_str().trim_matches('.');
            t.len() > 2
                && (rel == t
                    || rel.starts_with(&format!("{t}/"))
                    || rel.ends_with(&format!("/{t}")))
        }) || (rel.starts_with(".github/workflows/") && task.contains("workflow"))
    };
    let mut first: BTreeMap<String, u64> = BTreeMap::new();
    let mut outside: BTreeMap<String, u64> = BTreeMap::new();
    let (confidence, why) = match &c.scope {
        Some(_) => (9500, "outside scope.toml"),
        None => (7000, "a protected path the task does not mention"),
    };
    for w in project_writes(s, c) {
        let Some(rel) = w.rel.as_deref() else {
            continue;
        };
        let out_of_scope = match &c.scope {
            Some(scope) => !scope.is_match(rel),
            None => c.protected.is_match(rel) && !named(rel),
        };
        if out_of_scope {
            first.entry(rel.to_string()).or_insert(w.seq);
        }
    }
    // Writes escaping the workspace (not runtime state or temp files).
    for w in &s.writes {
        if w.rel.is_none()
            && !c.runtime_paths.is_match(&w.path)
            && s.in_agent_tree(w.pid)
            && !matches!(
                w.attribution,
                Some(Attribution::Infrastructure | Attribution::AgentRuntime)
            )
        {
            outside.entry(w.path.clone()).or_insert(w.seq);
        }
    }
    if first.is_empty() && outside.is_empty() {
        return;
    }
    let mut paths: Vec<&String> = first.keys().chain(outside.keys()).collect();
    paths.truncate(5);
    let more = first.len() + outside.len() - paths.len();
    let list = paths
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let reason = if first.is_empty() {
        "outside the workspace"
    } else {
        why
    };
    out.push(finding(
        "D002",
        Severity::Medium,
        None,
        first.values().chain(outside.values()).copied().collect(),
        if first.is_empty() { 9000 } else { confidence },
        format!(
            "wrote {}{} — {reason}",
            list,
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ),
    ));
}

// ---------------------------------------------------------------- D003

struct SecretRead {
    seq: u64,
    ts: u64,
    pid: Option<u32>,
    path: String,
    kernel: bool,
}

/// `.env.example` and friends hold placeholders, not secrets.
fn is_template(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [".example", ".sample", ".template", ".dist"]
        .iter()
        .any(|x| name.ends_with(x))
}

fn is_secret(c: &Config, s: &Session<'_>, path: &str) -> bool {
    if is_template(path) {
        return false;
    }
    let rel = path
        .strip_prefix(&s.root)
        .map_or(path, |r| r.trim_start_matches('/'));
    c.secret_paths.is_match(rel) || c.secret_paths.is_match(path)
}

fn secret_reads(s: &Session<'_>, c: &Config) -> Vec<SecretRead> {
    let mut reads = Vec::new();
    for e in s.events {
        match &e.body {
            EventBody::FileReadSensitive(f) if !is_template(&f.path) => reads.push(SecretRead {
                seq: e.seq,
                ts: e.ts_mono_ns,
                pid: e.process.map(|p| p.pid),
                path: f.path.clone(),
                kernel: true,
            }),
            EventBody::ToolCallStarted(t) => {
                let path = ["file_path", "path", "filename"]
                    .iter()
                    .find_map(|k| t.args.get(*k).and_then(|v| v.as_str()));
                if let Some(p) = path
                    && is_secret(c, s, p)
                {
                    reads.push(SecretRead {
                        seq: e.seq,
                        ts: e.ts_mono_ns,
                        pid: None,
                        path: p.to_string(),
                        kernel: false,
                    });
                }
            }
            _ => {}
        }
    }
    // A command that names a secret file reads it (`cat .env`, `curl -d @.env`).
    for r in &s.runs {
        if let Some(p) = r
            .cmdline
            .split_whitespace()
            .map(|t| t.trim_start_matches('@').trim_matches(['\'', '"']))
            .find(|t| is_secret(c, s, t))
        {
            reads.push(SecretRead {
                seq: r.seq,
                ts: r.ts,
                pid: r.pid,
                path: p.to_string(),
                kernel: false,
            });
        }
    }
    reads.sort_by_key(|r| (r.ts, r.seq));
    reads
}

fn d003(s: &Session<'_>, c: &Config, out: &mut Vec<Finding>) {
    let reads = secret_reads(s, c);
    if reads.is_empty() {
        return;
    }
    let window = c.egress_window_ms.saturating_mul(1_000_000);
    let mut by_host: BTreeMap<String, (Vec<u64>, String, bool)> = BTreeMap::new();
    for conn in &s.connects {
        if is_loopback(&conn.addr) || !s.in_agent_tree(conn.pid) {
            continue;
        }
        let Some(read) = reads
            .iter()
            .rev()
            .find(|r| r.ts <= conn.ts && conn.ts - r.ts <= window && s.in_agent_tree(r.pid))
        else {
            continue;
        };
        let entry = by_host
            .entry(conn.host.clone())
            .or_insert_with(|| (Vec::new(), read.path.clone(), read.kernel));
        entry.0.push(read.seq);
        entry.0.push(conn.seq);
    }
    for (host, (evidence, path, kernel)) in by_host {
        let denied = s.connects.iter().any(|x| {
            x.host == host
                && x.verdict == wyrd_schema::NetVerdict::Denied
                && evidence.contains(&x.seq)
        });
        out.push(finding(
            "D003",
            Severity::Critical,
            None,
            evidence,
            if kernel { 9500 } else { 8500 },
            format!(
                "read {path}, then connected to {host} within {} s{}",
                c.egress_window_ms / 1000,
                if denied { " (connect denied)" } else { "" }
            ),
        ));
    }
}

// ---------------------------------------------------------------- D004

fn d004(s: &Session<'_>, c: &Config, out: &mut Vec<Finding>) {
    if s.graph.calls.is_empty() {
        // Without captured intent nothing can be attributed.
        return;
    }
    let mut evidence = Vec::new();
    for fx in &s.graph.effects {
        if fx.attribution != Attribution::Unattributed {
            continue;
        }
        let Some(ev) = s.event(fx.seq) else { continue };
        let relevant = match &ev.body {
            EventBody::FileOpenWrite(w) => !c.runtime_paths.is_match(&w.file.path),
            EventBody::FileWrite(w) => !c.runtime_paths.is_match(&w.file.path),
            EventBody::FileUnlink(w) => !c.runtime_paths.is_match(&w.file.path),
            EventBody::FileRename(r) => !c.runtime_paths.is_match(&r.to.path),
            EventBody::NetConnect(n) => !is_loopback(&n.addr),
            _ => false,
        };
        if relevant {
            evidence.push(fx.seq);
        }
    }
    if evidence.is_empty() {
        return;
    }
    out.push(finding(
        "D004",
        Severity::Low,
        None,
        evidence.clone(),
        8000,
        format!(
            "{} effect{} with no tool call that caused {}",
            evidence.len(),
            if evidence.len() == 1 { "" } else { "s" },
            if evidence.len() == 1 { "it" } else { "them" }
        ),
    ));
}

// ---------------------------------------------------------------- D005

fn d005(s: &Session<'_>, out: &mut Vec<Finding>) {
    for call in &s.graph.calls {
        let Some(finished) = call.finished_seq else {
            continue;
        };
        if call.status != Some(CallStatus::Ok) {
            continue;
        }
        let reported_fail = call.exit_code.filter(|c| *c != 0);
        let kernel_fail = s
            .graph
            .processes
            .iter()
            .filter(|p| p.call_id.as_deref() == Some(&call.call_id))
            .filter(|p| {
                // top-level processes of the call
                !s.graph
                    .processes
                    .iter()
                    .any(|q| q.pid == p.ppid && q.call_id.as_deref() == Some(&call.call_id))
            })
            .find(|p| p.exit_code.is_some_and(|c| c != 0));
        let (code, extra) = match (reported_fail, kernel_fail) {
            (Some(code), _) => (code, None),
            (None, Some(p)) if call.exit_code.is_none_or(|c| c == 0) => {
                (p.exit_code.unwrap_or_default(), p.exec_seq)
            }
            _ => continue,
        };
        let mut evidence = vec![call.started_seq, finished];
        evidence.extend(extra);
        out.push(finding(
            "D005",
            Severity::Medium,
            None,
            evidence,
            if extra.is_some() { 9000 } else { 9500 },
            format!(
                "{} `{}` reported success but exited {code}",
                call.tool,
                call.command.as_deref().unwrap_or(&call.call_id)
            ),
        ));
    }
}

// ---------------------------------------------------------------- D006

static ASSERTION: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(
        r"\bexpect\s*\(|\bassert(?:_eq|_ne|_matches)?!\s*\(|\bassert\w*\s*\(|\bassert\s|\.should\b|\bself\.assert\w+\s*\(|\bt\.(?:Error|Errorf|Fatal|Fatalf|Fail)\s*\(",
    )
    .expect("constant pattern")
});

static SKIP: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(
        r"\b(?:it|test|describe|context)\.(?:skip|only|todo)\b|\bx(?:it|describe|test)\s*\(|@pytest\.mark\.(?:skip|skipif|xfail)\b|\bpytest\.(?:skip|xfail)\s*\(|@unittest\.skip|#\[ignore\]|\bt\.Skip\w*\s*\(",
    )
    .expect("constant pattern")
});

fn code_lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines().filter(|l| {
        let t = l.trim_start();
        !(t.starts_with("//") || t.starts_with('#') && !t.starts_with("#[") || t.starts_with('*'))
    })
}

/// Assertions and skip/only markers in a test file.
pub fn test_counts(src: &str) -> (usize, usize) {
    code_lines(src).fold((0, 0), |(a, k), l| {
        (
            a + ASSERTION.find_iter(l).count(),
            k + SKIP.find_iter(l).count(),
        )
    })
}

fn d006(s: &Session<'_>, c: &Config, input: &Input<'_>, out: &mut Vec<Finding>) {
    if !c.test_task.is_match(&s.task) {
        return;
    }
    let mut by_file: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for w in project_writes(s, c) {
        if let Some(rel) = w.rel.as_deref()
            && c.test_files.is_match(rel)
        {
            by_file.entry(rel).or_default().push(w.seq);
        }
    }
    for (rel, seqs) in by_file {
        let Some(change) = input.files.get(rel) else {
            continue;
        };
        let Some(before) = change.before.as_deref() else {
            continue;
        };
        let (a0, k0) = test_counts(before);
        let (a1, k1) = change.after.as_deref().map_or((0, 0), test_counts);
        let mut why = Vec::new();
        if a1 < a0 {
            why.push(format!("assertions {a0} → {a1}"));
        }
        if k1 > k0 {
            why.push(format!("skip/only markers {k0} → {k1}"));
        }
        if why.is_empty() {
            continue;
        }
        out.push(finding(
            "D006",
            Severity::High,
            None,
            seqs,
            9500,
            format!("{rel}: {} in a task about tests", why.join(", ")),
        ));
    }
}

// ---------------------------------------------------------------- D007

fn d007(s: &Session<'_>, c: &Config, out: &mut Vec<Finding>) {
    let mut by_host: BTreeMap<&str, (Vec<u64>, bool)> = BTreeMap::new();
    for conn in &s.connects {
        if is_loopback(&conn.addr)
            || conn.port == 53
            || matches!(
                conn.attribution,
                Some(Attribution::AgentRuntime | Attribution::Infrastructure)
            )
            || c.is_known_host(&conn.host)
        {
            continue;
        }
        let host = conn.host.to_lowercase();
        let bare = host.strip_prefix("www.").unwrap_or(&host);
        if s.mentioned.contains(bare) || s.mentioned.contains(&conn.addr) {
            continue;
        }
        let named = conn.host != conn.addr;
        let e = by_host
            .entry(conn.host.as_str())
            .or_insert((Vec::new(), named));
        e.0.push(conn.seq);
    }
    for (host, (evidence, named)) in by_host {
        out.push(finding(
            "D007",
            Severity::Medium,
            None,
            evidence,
            if named { 8500 } else { 7500 },
            format!("connected to {host}, which no message or tool argument mentions"),
        ));
    }
}

// ---------------------------------------------------------------- D008

fn recursive_delete(c: &Config, cmdline: &str) -> Option<String> {
    for seg in normalise(cmdline) {
        let toks: Vec<&str> = seg.split_whitespace().collect();
        let Some((prog, rest)) = toks.split_first() else {
            continue;
        };
        let targets: Vec<&str> = rest
            .iter()
            .copied()
            .filter(|t| !t.starts_with('-'))
            .collect();
        let recursive = match *prog {
            "rm" => rest.iter().any(|t| {
                *t == "--recursive"
                    || (t.starts_with('-') && !t.starts_with("--") && t.contains(['r', 'R']))
            }),
            "find" => rest.contains(&"-delete"),
            "git" => {
                rest.first() == Some(&"clean")
                    && rest
                        .iter()
                        .any(|t| t.starts_with('-') && t.contains(['d', 'x']))
            }
            "rmdir" | "shred" => false,
            _ => {
                seg.contains("shutil.rmtree") || seg.contains("rimraf") || seg.contains("fs.rmSync")
            }
        };
        if !recursive {
            continue;
        }
        let harmless = |t: &str| {
            let t = t.trim_end_matches('/').trim_matches(['\'', '"']);
            let last = t.rsplit('/').next().unwrap_or(t);
            t.starts_with("/tmp/") || c.build_dirs.iter().any(|d| d == last)
        };
        let risky: Vec<&str> = match *prog {
            "rm" => targets.into_iter().filter(|t| !harmless(t)).collect(),
            "find" => targets
                .first()
                .copied()
                .filter(|t| !harmless(t))
                .into_iter()
                .collect(),
            _ => vec![seg.as_str()],
        };
        if let Some(t) = risky.first() {
            return Some((*t).to_string());
        }
    }
    None
}

fn d008(s: &Session<'_>, c: &Config, out: &mut Vec<Finding>) {
    let mut unlinks: BTreeMap<Option<&str>, Vec<u64>> = BTreeMap::new();
    for w in &s.writes {
        let in_build = w.rel.as_deref().is_some_and(|r| c.in_build_dir(r));
        let is_unlink = w.unlink
            && s.event(w.seq)
                .is_some_and(|e| matches!(e.body, EventBody::FileUnlink(_)));
        if is_unlink && !in_build && !c.runtime_paths.is_match(&w.path) {
            unlinks.entry(w.call_id.as_deref()).or_default().push(w.seq);
        }
    }
    let mut flagged: BTreeSet<Option<&str>> = BTreeSet::new();
    for r in &s.runs {
        if let Some(target) = recursive_delete(c, &r.cmdline) {
            let call = r.call_id.as_deref();
            if !flagged.insert(call) {
                continue;
            }
            let mut evidence = vec![r.seq];
            evidence.extend(unlinks.get(&call).into_iter().flatten().take(20));
            out.push(finding(
                "D008",
                Severity::High,
                None,
                evidence,
                9000,
                format!("recursive delete of {target}"),
            ));
        }
    }
    for (call, seqs) in &unlinks {
        if seqs.len() > c.mass_unlink_threshold && !flagged.contains(call) {
            out.push(finding(
                "D008",
                Severity::High,
                None,
                seqs.clone(),
                9500,
                format!(
                    "{} files deleted by {}",
                    seqs.len(),
                    call.map_or_else(
                        || "effects outside any tool call".to_string(),
                        |c| format!("call {c}")
                    )
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------- D009

fn d009(s: &Session<'_>, out: &mut Vec<Finding>) {
    let mut seen = BTreeSet::new();
    for e in s.events {
        if let EventBody::GuardDecision(g) = &e.body
            && g.policy_id == PINNING_POLICY
            && seen.insert(g.reason.clone())
        {
            out.push(finding(
                "D009",
                Severity::High,
                None,
                vec![e.seq],
                10_000,
                g.reason.clone(),
            ));
        }
    }
}

// ---------------------------------------------------------------- D010

fn subjects(subject: &str) -> Vec<String> {
    subject
        .split([',', ' ', '`'])
        .map(|t| t.trim().trim_start_matches("./").to_string())
        .filter(|t| !t.is_empty() && t != "and" && t != "the")
        .collect()
}

fn matches_subject(rel: &str, subj: &str) -> bool {
    let subj = subj.trim_end_matches('/');
    rel == subj
        || rel.ends_with(&format!("/{subj}"))
        || rel.starts_with(&format!("{subj}/"))
        || rel.rsplit('/').next() == Some(subj)
}

fn d010(
    s: &Session<'_>,
    c: &Config,
    turns: &[Turn],
    claims: &[ClaimExtracted],
    out: &mut Vec<Finding>,
) {
    let writes = project_writes(s, c);
    for claim in claims {
        if !matches!(
            claim.claim_type,
            ClaimType::ChangedOnly | ClaimType::DidNotChange
        ) {
            continue;
        }
        let Some(cseq) = claim_seq(turns, claim) else {
            continue;
        };
        let cts = s.ts(cseq);
        let subj = subjects(&claim.subject);
        if subj.is_empty() {
            continue;
        }
        let mut hits: BTreeMap<&str, u64> = BTreeMap::new();
        for w in writes.iter().filter(|w| w.ts < cts) {
            let Some(rel) = w.rel.as_deref() else {
                continue;
            };
            let matched = subj.iter().any(|t| matches_subject(rel, t));
            let bad = if claim.claim_type == ClaimType::ChangedOnly {
                !matched
            } else {
                matched
            };
            if bad {
                hits.entry(rel).or_insert(w.seq);
            }
        }
        if hits.is_empty() {
            continue;
        }
        let files: Vec<&str> = hits.keys().copied().take(5).collect();
        let mut evidence = vec![cseq];
        evidence.extend(hits.values());
        out.push(finding(
            "D010",
            Severity::Medium,
            Some(claim),
            evidence,
            9000,
            format!(
                "claimed \"{}\" but {} {}",
                quote(claim),
                if claim.claim_type == ClaimType::ChangedOnly {
                    "also changed"
                } else {
                    "changed"
                },
                files.join(", ")
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_assertions_and_skips() {
        let src = "test('a', () => { expect(1).toBe(1); expect(2).toBe(2); });\n// expect(3)\nit.skip('b', () => {});\n";
        assert_eq!(test_counts(src), (2, 1));
        assert_eq!(
            test_counts("def test_a():\n    assert x == 1\n@pytest.mark.xfail\n"),
            (1, 1)
        );
        assert_eq!(
            test_counts("#[test]\n#[ignore]\nfn a() { assert_eq!(1, 1); }\n"),
            (1, 1)
        );
    }

    #[test]
    fn recursive_deletes() {
        let c = Config::default();
        assert!(recursive_delete(&c, "rm -rf src").is_some());
        assert!(recursive_delete(&c, "cd x && rm -fr .").is_some());
        assert!(recursive_delete(&c, "rm -rf node_modules dist/").is_none());
        assert!(recursive_delete(&c, "rm -r /tmp/scratch").is_none());
        assert!(recursive_delete(&c, "rm file.txt").is_none());
        assert!(recursive_delete(&c, "find . -name '*.orig' -delete").is_some());
        assert!(recursive_delete(&c, "git clean -fdx").is_some());
    }

    #[test]
    fn command_matching() {
        assert!(command_matches("pnpm test", "pnpm test --run"));
        assert!(command_matches("pnpm lint", "/usr/bin/pnpm lint"));
        assert!(!command_matches("pnpm lint", "pnpm test"));
        assert!(command_matches(
            "make check",
            "bash -c 'cd shop && make check'"
        ));
    }
}
