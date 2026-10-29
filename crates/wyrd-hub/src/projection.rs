//! What the analytics projection of a bundle contains (one row per
//! event, divergence, guard decision, and judge run), the stats computed
//! over projections, and the search query language. Pure functions;
//! `analytics` writes and queries them with DuckDB.

use serde::{Deserialize, Serialize};
use wyrd_bundle::Bundle;
use wyrd_schema::{Decision, EventBody, NetVerdict};

/// Longest text kept per column (search needs context, not documents).
const MAX_TEXT: usize = 300;

fn clip(s: &str) -> String {
    if s.len() <= MAX_TEXT {
        return s.to_string();
    }
    let mut end = MAX_TEXT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// One event, flattened for analytics and search.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventRow {
    pub seq: i64,
    pub plane: String,
    pub kind: String,
    pub ts_wall_ms: i64,
    pub pid: Option<i64>,
    /// File path of file effects (redacted, `~` for the home directory).
    pub path: Option<String>,
    /// Host, executable, tool, policy, or rule the event is about.
    pub target: Option<String>,
    pub detail: Option<String>,
    /// `denied` for refused connects, denied tool calls, and deny decisions.
    pub outcome: Option<String>,
    pub drops: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergenceRow {
    pub seq: i64,
    pub rule_id: String,
    pub severity: String,
    pub confidence_bp: i32,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardRow {
    pub seq: i64,
    pub stage: String,
    pub decision: String,
    pub policy_id: String,
    pub reason: String,
}

/// Token usage of one judge run, as `wyrd verdict` records it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// One LLM claim-extraction run of a session (`judge-runs.jsonl` next
/// to the local ledger; sent with the upload as `judge_runs`). Fields the
/// Hub does not use are dropped; missing numbers are zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeRun {
    pub model: String,
    #[serde(default)]
    pub prompt_version: String,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub est_cost_usd: f64,
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
}

/// Largest `judge_runs` field accepted, in bytes and lines.
pub const MAX_JUDGE_BYTES: usize = 1 << 20;
pub const MAX_JUDGE_RUNS: usize = 1000;

/// Parse the `judge_runs` upload field (JSON lines). The message names
/// the first bad line.
pub fn parse_judge_runs(text: &str) -> Result<Vec<JudgeRun>, String> {
    if text.len() > MAX_JUDGE_BYTES {
        return Err(format!("judge_runs: larger than {MAX_JUDGE_BYTES} bytes"));
    }
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if out.len() == MAX_JUDGE_RUNS {
            return Err(format!("judge_runs: more than {MAX_JUDGE_RUNS} runs"));
        }
        let run: JudgeRun =
            serde_json::from_str(line).map_err(|e| format!("judge_runs: line {}: {e}", i + 1))?;
        if run.model.is_empty() || run.model.len() > 200 || !run.est_cost_usd.is_finite() {
            return Err(format!("judge_runs: line {}: invalid run", i + 1));
        }
        out.push(run);
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    pub events: Vec<EventRow>,
    pub divergences: Vec<DivergenceRow>,
    pub guard: Vec<GuardRow>,
    pub judge: Vec<JudgeRow>,
}

/// One judge run, flattened for analytics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeRow {
    pub model: String,
    pub prompt_version: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    /// Estimated cost in micro-dollars (integers keep sums exact).
    pub cost_micro_usd: i64,
    pub latency_ms: i64,
    pub failed: bool,
}

fn tokens(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub fn judge_rows(runs: &[JudgeRun]) -> Vec<JudgeRow> {
    runs.iter()
        .map(|r| JudgeRow {
            model: r.model.clone(),
            prompt_version: r.prompt_version.clone(),
            input_tokens: tokens(r.usage.input_tokens),
            output_tokens: tokens(r.usage.output_tokens),
            cache_creation_input_tokens: tokens(r.usage.cache_creation_input_tokens),
            cache_read_input_tokens: tokens(r.usage.cache_read_input_tokens),
            #[allow(clippy::cast_possible_truncation)] // clamped to i64 by `as` on a finite f64
            cost_micro_usd: (r.est_cost_usd.max(0.0) * 1_000_000.0).round() as i64,
            latency_ms: tokens(r.latency_ms),
            failed: r.error.is_some(),
        })
        .collect()
}

/// Divergence counts per rule: (rule, sessions with it, times raised).
pub type RuleCount = (String, i64, i64);

/// Judge usage over a set of projections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JudgeStats {
    pub runs: i64,
    pub failed: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cost_micro_usd: i64,
    /// Per model: runs and cost in micro-dollars.
    pub by_model: Vec<(String, i64, i64)>,
}

/// What `GET …/stats` reports from the projections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub rules: Vec<RuleCount>,
    /// Denied actions by event kind.
    pub denied: Vec<(String, i64)>,
    pub events: i64,
    pub dropped: i64,
    pub sensor_lost: i64,
    pub judge: JudgeStats,
}

fn snake<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The projection rows of a verified bundle and the judge runs sent
/// with it.
pub fn rows(bundle: &Bundle, judge: &[JudgeRun]) -> Rows {
    let mut out = Rows {
        judge: judge_rows(judge),
        ..Rows::default()
    };
    for e in &bundle.events {
        let seq = i64::try_from(e.seq).unwrap_or(i64::MAX);
        let mut r = EventRow {
            seq,
            plane: snake(&e.plane()),
            kind: e.body.kind().to_string(),
            ts_wall_ms: e.ts_wall_ms,
            pid: e.process.as_ref().map(|p| i64::from(p.pid)),
            drops: i64::from(e.drops_since_last),
            ..EventRow::default()
        };
        match &e.body {
            EventBody::FileOpenWrite(f) => r.path = Some(f.file.path.clone()),
            EventBody::FileWrite(f) => r.path = Some(f.file.path.clone()),
            EventBody::FileUnlink(f) => r.path = Some(f.file.path.clone()),
            EventBody::FileChmod(f) => r.path = Some(f.file.path.clone()),
            EventBody::FileTruncate(f) => r.path = Some(f.file.path.clone()),
            EventBody::FileMkdir(f) | EventBody::FileReadSensitive(f) => {
                r.path = Some(f.path.clone());
            }
            EventBody::FileRename(f) => {
                r.path = Some(f.to.path.clone());
                r.detail = Some(clip(&format!("{} -> {}", f.from.path, f.to.path)));
            }
            EventBody::NetConnect(n) => {
                let host = n.dns_name.clone().unwrap_or_else(|| n.addr.clone());
                r.target = Some(format!("{host}:{}", n.port));
                r.detail = Some(n.addr.clone());
                if n.verdict == NetVerdict::Denied {
                    r.outcome = Some("denied".into());
                }
            }
            EventBody::DnsQuery(d) => r.target = Some(d.name.clone()),
            EventBody::ProcExec(p) => {
                r.target = Some(p.filename.clone());
                r.detail = Some(clip(&p.argv.join(" ")));
            }
            EventBody::ToolCallStarted(t) => {
                r.target = Some(t.tool.clone());
                r.detail = Some(clip(&t.args.to_string()));
            }
            EventBody::ToolCallDenied(t) => {
                r.target = Some(t.policy_id.clone());
                r.detail = Some(clip(&t.reason));
                r.outcome = Some("denied".into());
            }
            EventBody::Message(m) => r.detail = Some(clip(&m.text)),
            EventBody::GuardDecision(g) => {
                r.target = Some(g.policy_id.clone());
                r.detail = Some(clip(&g.reason));
                if g.decision == Decision::Deny {
                    r.outcome = Some("denied".into());
                }
                out.guard.push(GuardRow {
                    seq,
                    stage: snake(&g.stage),
                    decision: snake(&g.decision),
                    policy_id: g.policy_id.clone(),
                    reason: clip(&g.reason),
                });
            }
            EventBody::DivergenceRaised(d) => {
                r.target = Some(d.rule_id.clone());
                r.detail = Some(clip(&d.summary));
                out.divergences.push(DivergenceRow {
                    seq,
                    rule_id: d.rule_id.clone(),
                    severity: snake(&d.severity),
                    confidence_bp: i32::from(d.confidence_bp),
                    summary: clip(&d.summary),
                });
            }
            _ => {}
        }
        out.events.push(r);
    }
    out
}

/// A parsed search query. Every filter must hold (AND); several values
/// of one key must all match too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub paths: Vec<String>,
    pub rules: Vec<String>,
    pub repos: Vec<String>,
    pub agents: Vec<String>,
    pub sessions: Vec<String>,
    pub kinds: Vec<String>,
    pub hosts: Vec<String>,
    pub exes: Vec<String>,
    pub tools: Vec<String>,
    /// Words without a key: matched against path, target, and detail.
    pub text: Vec<String>,
}

impl Query {
    /// Filters that need the events (not just bundle metadata).
    pub fn needs_events(&self) -> bool {
        !(self.paths.is_empty()
            && self.kinds.is_empty()
            && self.hosts.is_empty()
            && self.exes.is_empty()
            && self.tools.is_empty()
            && self.text.is_empty())
    }
}

/// A malformed query, with the character position of the problem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at position {position}")]
pub struct QueryError {
    pub position: usize,
    pub message: String,
}

pub const KEYS: &[&str] = &[
    "path", "rule", "repo", "agent", "session", "kind", "host", "exe", "tool",
];
const MAX_QUERY: usize = 500;
const MAX_TERMS: usize = 20;

fn err(position: usize, message: impl Into<String>) -> QueryError {
    QueryError {
        position,
        message: message.into(),
    }
}

/// Parse `path:~/.ssh rule:D003 repo:shop "two words" curl`.
pub fn parse(input: &str) -> Result<Query, QueryError> {
    let chars: Vec<char> = input.chars().collect();
    if chars.len() > MAX_QUERY {
        return Err(err(
            MAX_QUERY,
            format!("query longer than {MAX_QUERY} characters"),
        ));
    }
    let mut out = Query::default();
    let mut i = 0;
    let mut terms = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        terms += 1;
        if terms > MAX_TERMS {
            return Err(err(start, format!("more than {MAX_TERMS} terms")));
        }
        // An optional `key:` prefix (`word://` is a URL, not a key).
        let mut key = None;
        let mut end = i;
        while end < chars.len() && chars[end].is_ascii_alphabetic() {
            end += 1;
        }
        if end > i && chars.get(end) == Some(&':') && chars.get(end + 1) != Some(&'/') {
            let k: String = chars[i..end].iter().collect();
            if !KEYS.contains(&k.as_str()) {
                return Err(err(
                    start,
                    format!("unknown filter {k:?} (use one of {})", KEYS.join(", ")),
                ));
            }
            key = Some(k);
            i = end + 1;
        }
        let value_start = i;
        let value: String = if i < chars.len() && chars[i] == '"' {
            i += 1;
            let mut quoted = String::new();
            loop {
                match chars.get(i) {
                    None => return Err(err(value_start, "unterminated quote")),
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some('\\') if chars.get(i + 1).is_some() => {
                        quoted.push(chars[i + 1]);
                        i += 2;
                    }
                    Some(c) => {
                        quoted.push(*c);
                        i += 1;
                    }
                }
            }
            if i < chars.len() && !chars[i].is_whitespace() {
                return Err(err(i, "expected a space after the closing quote"));
            }
            quoted
        } else {
            let from = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                if chars[i] == '"' {
                    return Err(err(i, "quote inside a word"));
                }
                i += 1;
            }
            chars[from..i].iter().collect()
        };
        if value.is_empty() {
            return Err(err(value_start, "empty value"));
        }
        let list = match key.as_deref() {
            None => &mut out.text,
            Some("path") => &mut out.paths,
            Some("rule") => &mut out.rules,
            Some("repo") => &mut out.repos,
            Some("agent") => &mut out.agents,
            Some("session") => &mut out.sessions,
            Some("kind") => &mut out.kinds,
            Some("host") => &mut out.hosts,
            Some("exe") => &mut out.exes,
            Some(_) => &mut out.tools,
        };
        list.push(value);
    }
    if terms == 0 {
        return Err(err(0, "empty query"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_filters_quotes_and_words() {
        let q = parse(r#"path:~/.ssh rule:D003 repo:shop "two words" curl kind:"effect.net.connect" host:a\b"#)
            .unwrap();
        assert_eq!(q.paths, ["~/.ssh"]);
        assert_eq!(q.rules, ["D003"]);
        assert_eq!(q.repos, ["shop"]);
        assert_eq!(q.kinds, ["effect.net.connect"]);
        assert_eq!(q.hosts, [r"a\b"]);
        assert_eq!(q.text, ["two words", "curl"]);
        assert!(q.needs_events());
        assert!(!parse("rule:D001 repo:x").unwrap().needs_events());
        assert_eq!(parse(r#"path:"a \"b\" c""#).unwrap().paths, [r#"a "b" c"#]);
        // A colon later in a word is just text.
        assert_eq!(parse("http://x").unwrap().text, ["http://x"]);
    }

    #[test]
    fn errors_carry_the_position() {
        let e = |q: &str| parse(q).unwrap_err();
        assert_eq!(e("rule:D1 color:red").position, 8);
        assert!(e("rule:D1 color:red").message.contains("unknown filter"));
        assert_eq!(e("path:").position, 5);
        assert_eq!(e("path:\"abc").position, 5);
        assert_eq!(e("ab\"c").position, 2);
        assert_eq!(e("path:\"a\"b").position, 8);
        assert_eq!(e("   ").position, 0);
        assert_eq!(e(&"x ".repeat(21)).position, 40);
        assert_eq!(e(&"x".repeat(501)).position, 500);
    }

    #[test]
    fn judge_runs_parse_with_limits() {
        let ok = parse_judge_runs(concat!(
            r#"{"model":"m","prompt_version":"v1","usage":{"input_tokens":100,"output_tokens":20},"est_cost_usd":0.0015,"latency_ms":50,"attempts":1,"dropped":0,"error":null}"#,
            "\n\n",
            r#"{"model":"n","error":"timeout"}"#,
            "\n"
        ))
        .unwrap();
        assert_eq!(ok.len(), 2);
        let rows = judge_rows(&ok);
        assert_eq!(rows[0].cost_micro_usd, 1500);
        assert_eq!(rows[0].input_tokens, 100);
        assert!(!rows[0].failed && rows[1].failed);
        assert_eq!(parse_judge_runs("").unwrap(), []);
        let e = |t: &str| parse_judge_runs(t).unwrap_err();
        assert!(e("{\"model\":\"m\"}\nnot json").contains("line 2"));
        assert!(e("{\"model\":\"\"}").contains("invalid run"));
        assert!(e(&"{\"model\":\"m\"}\n".repeat(1001)).contains("more than"));
        assert!(e(&" ".repeat(MAX_JUDGE_BYTES + 1)).contains("larger than"));
    }

    #[test]
    fn clips_on_char_boundaries() {
        let s = "é".repeat(200);
        let c = clip(&s);
        assert!(c.len() <= MAX_TEXT && s.starts_with(&c));
    }
}
