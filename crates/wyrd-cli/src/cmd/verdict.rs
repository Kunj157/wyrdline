//! Divergences: computed when `wyrd run` ends (`record`), listed with
//! `wyrd divergences`, triaged with `wyrd triage`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use wyrd_cell::{DataDir, Session};
use wyrd_divergence::view::{DivergenceView, project, recorded_ids};
use wyrd_divergence::{Config, FileChange};
use wyrd_schema::{
    CaptureMode, DivergenceStatus, DivergenceUpdated, Event, EventBody, FileOpenWrite, FilePath,
};

use crate::exit;

/// Largest test file read for D006.
const MAX_TEST_FILE: usize = 1 << 20;

#[derive(Debug, clap::Args)]
pub struct DivergencesArgs {
    /// Session id or unique prefix.
    session: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct TriageArgs {
    /// Session id or unique prefix.
    session: String,
    /// Divergence id (`dv-…`).
    divergence: String,
    /// Mark as seen and real.
    #[arg(long, conflicts_with_all = ["false_positive", "reopen"])]
    ack: bool,
    /// Mark as a false positive.
    #[arg(long)]
    false_positive: bool,
    /// Open again.
    #[arg(long)]
    reopen: bool,
    /// Why (stored in the ledger, keep it free of secrets).
    #[arg(long)]
    note: Option<String>,
}

/// The project's `.wyrdline/` configuration; defaults where files are
/// missing. A malformed file is an error, not silently ignored.
pub fn project_config(workspace: &Path) -> anyhow::Result<Config> {
    let read = |name: &str| -> anyhow::Result<Option<String>> {
        match std::fs::read_to_string(workspace.join(".wyrdline").join(name)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    };
    Ok(Config::from_project(
        read("config.toml")?.as_deref(),
        read("scope.toml")?.as_deref(),
        read("secrets.toml")?.as_deref(),
    )?)
}

fn written_paths(events: &[Event], root: &str) -> Vec<String> {
    let mut out: Vec<String> = events
        .iter()
        .flat_map(|e| match &e.body {
            EventBody::FileOpenWrite(FileOpenWrite { file, .. }) => vec![file],
            EventBody::FileWrite(w) => vec![&w.file],
            EventBody::FileTruncate(w) => vec![&w.file],
            EventBody::FileUnlink(u) => vec![&u.file],
            EventBody::FileRename(r) => vec![&r.from, &r.to],
            _ => Vec::new(),
        })
        .filter_map(|f: &FilePath| {
            f.path
                .strip_prefix(root)
                .and_then(|r| r.strip_prefix('/'))
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn text(bytes: Option<Vec<u8>>) -> Option<String> {
    bytes.and_then(|b| String::from_utf8(b).ok())
}

/// Before (real workspace, untouched by a Cell) and after (head of the
/// current branch) of every changed test file. Observe mode has no
/// "before", so D006 cannot judge it.
fn test_files(s: &Session, events: &[Event], config: &Config) -> BTreeMap<String, FileChange> {
    let mut out = BTreeMap::new();
    if s.record().mode != CaptureMode::Cell {
        return out;
    }
    let root = s.record().workspace.display().to_string();
    for rel in written_paths(events, &root) {
        if !config.test_files.is_match(&rel) {
            continue;
        }
        let real = s.record().workspace.join(&rel);
        let before = std::fs::read(&real)
            .ok()
            .filter(|b| b.len() <= MAX_TEST_FILE);
        let after = s
            .file_at("head", &rel, MAX_TEST_FILE)
            .ok()
            .filter(|f| !f.truncated)
            .and_then(|f| f.bytes);
        out.insert(
            rel,
            FileChange {
                before: text(before),
                after: text(after),
            },
        );
    }
    out
}

/// File in the session directory holding one JSON judge run per line.
pub const JUDGE_RUNS: &str = "judge-runs.jsonl";

/// The LLM extractor when one is configured: `LLM_PROVIDER=anthropic`
/// (the default when `ANTHROPIC_API_KEY` is set) or `none`.
fn llm_settings() -> Option<(
    wyrd_divergence::llm::AnthropicHttp,
    wyrd_divergence::llm::Settings,
)> {
    let key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let provider = std::env::var("LLM_PROVIDER")
        .unwrap_or_else(|_| if key.is_some() { "anthropic" } else { "none" }.into());
    if provider != "anthropic" {
        return None;
    }
    let base =
        std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into());
    let mut settings = wyrd_divergence::llm::Settings::default();
    if let Ok(m) = std::env::var("WYRD_CLAIM_MODEL") {
        settings.model = m;
    }
    if let Some(c) = std::env::var("WYRD_LLM_MAX_COST_USD")
        .ok()
        .and_then(|c| c.parse().ok())
    {
        settings.max_cost_usd = c;
    }
    Some((
        wyrd_divergence::llm::AnthropicHttp::new(
            &base,
            key.unwrap_or_default(),
            std::time::Duration::from_secs(60),
        ),
        settings,
    ))
}

fn call_summaries(events: &[Event]) -> Vec<wyrd_divergence::llm::CallSummary> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ToolCallStarted(t) => Some(wyrd_divergence::llm::CallSummary {
                tool: t.tool.clone(),
                args: t.args.to_string(),
            }),
            _ => None,
        })
        .collect()
}

/// Regex claims, plus the model's when an extractor is configured; the
/// judge run is appended to the session's `judge-runs.jsonl`.
fn extract_claims(s: &Session, events: &[Event]) -> Vec<wyrd_schema::ClaimExtracted> {
    use wyrd_divergence::claims;
    let turns = claims::assistant_turns(events);
    let rule = claims::extract_all(&turns);
    let Some((http, settings)) = llm_settings() else {
        return rule;
    };
    let x = wyrd_divergence::llm::extract(
        &http,
        &settings,
        &turns,
        &call_summaries(events),
        &std::thread::sleep,
    );
    if let Some(e) = &x.run.error {
        let _ = writeln!(
            std::io::stderr(),
            "wyrd: warning: claims partial (rules only): {e}"
        );
    }
    if let Ok(line) = serde_json::to_string(&x.run) {
        let path = s.dir().root().join(JUDGE_RUNS);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"));
    }
    claims::merge(&turns, rule.into_iter().chain(x.claims))
}

/// Judge runs of a session, oldest first.
pub fn judge_runs(s: &Session) -> Vec<wyrd_divergence::llm::JudgeRun> {
    std::fs::read_to_string(s.dir().root().join(JUDGE_RUNS))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Extract claims, run the rules, and append what is new to the ledger
/// (`verdict.claim.extracted`, `verdict.divergence.raised`). Returns the
/// number of divergences raised.
pub fn record(s: &mut Session, events: &[Event]) -> anyhow::Result<usize> {
    let config = project_config(&s.record().workspace)?;
    let graph = wyrd_correlator::correlate(events);
    let files = test_files(s, events, &config);
    let claims = extract_claims(s, events);
    let divergences = wyrd_divergence::evaluate(
        &wyrd_divergence::Input {
            events,
            graph: &graph,
            claims: &claims,
            files: &files,
        },
        &config,
    );
    let known = recorded_ids(events);
    let raised = divergences.len();
    let bodies: Vec<EventBody> = claims
        .into_iter()
        .filter(|c| !known.contains(&c.claim_id))
        .map(EventBody::ClaimExtracted)
        .chain(
            divergences
                .into_iter()
                .filter(|d| !known.contains(&d.divergence_id))
                .map(EventBody::DivergenceRaised),
        )
        .collect();
    if !bodies.is_empty() {
        s.record_verdicts(bodies)?;
    }
    Ok(raised)
}

fn print(views: &[DivergenceView], json: bool, computed: bool) -> anyhow::Result<()> {
    let mut o = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut o, views)?;
        writeln!(o)?;
        return Ok(());
    }
    if views.is_empty() {
        writeln!(o, "no divergences")?;
    }
    for v in views {
        let d = &v.divergence;
        writeln!(
            o,
            "{}  {} {:<24} {:<9} {:<14} {}",
            d.divergence_id,
            d.rule_id,
            v.rule_name,
            format!("{:?}", d.severity).to_lowercase(),
            match v.status {
                DivergenceStatus::Open => "open",
                DivergenceStatus::Acknowledged => "acknowledged",
                DivergenceStatus::FalsePositive => "false positive",
            },
            d.summary
        )?;
        let seqs: Vec<String> = d.evidence_seqs.iter().map(u64::to_string).collect();
        writeln!(o, "      evidence: seq {}", seqs.join(", "))?;
    }
    if computed {
        writeln!(
            o,
            "(computed now, rules only; none were recorded for this session)"
        )?;
    }
    Ok(())
}

pub fn list(a: &DivergencesArgs) -> anyhow::Result<u8> {
    let s = Session::open(&DataDir::from_env(), &a.session)?;
    let events = super::graph::load(&s.record().id)?;
    let recorded = events
        .iter()
        .any(|e| e.plane() == wyrd_schema::Plane::Verdict);
    if recorded {
        print(&project(&events), a.json, false)?;
        if !a.json
            && let Some(run) = judge_runs(&s).last()
        {
            writeln!(
                std::io::stdout(),
                "claims: {} ({} {}, {} in / {} out tokens, ${:.4})",
                if run.error.is_some() {
                    "partial"
                } else {
                    "complete"
                },
                run.model,
                run.prompt_version,
                run.usage.input_tokens
                    + run.usage.cache_read_input_tokens
                    + run.usage.cache_creation_input_tokens,
                run.usage.output_tokens,
                run.est_cost_usd
            )?;
        }
        return Ok(exit::OK);
    }
    let config = project_config(&s.record().workspace)?;
    let graph = wyrd_correlator::correlate(&events);
    let files = test_files(&s, &events, &config);
    let (claims, divergences) =
        wyrd_divergence::evaluate_rules_only(&events, &graph, &files, &config);
    let synthetic: Vec<Event> = claims
        .into_iter()
        .map(EventBody::ClaimExtracted)
        .chain(divergences.into_iter().map(EventBody::DivergenceRaised))
        .map(|body| Event {
            seq: 0,
            ts_mono_ns: 0,
            ts_wall_ms: 0,
            process: None,
            drops_since_last: 0,
            body,
        })
        .collect();
    print(&project(&synthetic), a.json, true)?;
    Ok(exit::OK)
}

pub fn triage(a: &TriageArgs) -> anyhow::Result<u8> {
    let status = if a.ack {
        DivergenceStatus::Acknowledged
    } else if a.false_positive {
        DivergenceStatus::FalsePositive
    } else if a.reopen {
        DivergenceStatus::Open
    } else {
        anyhow::bail!("say --ack, --false-positive, or --reopen");
    };
    let mut s = Session::open(&DataDir::from_env(), &a.session)?;
    let events = super::graph::load(&s.record().id)?;
    if !project(&events)
        .iter()
        .any(|v| v.divergence.divergence_id == a.divergence)
    {
        anyhow::bail!(
            "no divergence {} in session {}",
            a.divergence,
            s.record().id
        );
    }
    match s.record_verdicts(vec![EventBody::DivergenceUpdated(DivergenceUpdated {
        divergence_id: a.divergence.clone(),
        status,
        note: a.note.clone(),
    })]) {
        Ok(seq) => {
            writeln!(std::io::stdout(), "recorded at seq {seq}")?;
            Ok(exit::OK)
        }
        Err(wyrd_cell::CellError::Busy(why)) => {
            writeln!(std::io::stderr(), "wyrd: {why}")?;
            Ok(exit::CONFLICT)
        }
        Err(e) => Err(e.into()),
    }
}
