//! Divergence and claim evaluation (plan §18.3).
//!
//! `wyrd-evals [--label vX.Y.Z] [--write-baseline]` runs every labelled
//! divergence case through the correlator and the rules, and every
//! labelled assistant turn through the claim extractors, writes
//! `docs/results/evals-<label>.{md,json}`, and exits 1 when a metric falls
//! below `docs/results/evals-baseline.json`.
//!
//! The LLM extractor is measured from recorded responses in
//! `tests/evals/claims/recorded/<prompt version>.json` (no network); with
//! `EVAL_LIVE=1` and `ANTHROPIC_API_KEY` it calls the API and records them.
#![allow(clippy::print_stdout, clippy::cast_precision_loss)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wyrd_divergence::case::Case;
use wyrd_divergence::claims::{self, Turn};
use wyrd_divergence::llm::{self, Reply, Transport};
use wyrd_divergence::{Input, RULES, evaluate};
use wyrd_schema::{ClaimExtracted, ClaimType};

#[derive(Debug, Deserialize)]
struct LabelledTurn {
    id: String,
    text: String,
    claims: Vec<Label>,
}

#[derive(Debug, Deserialize)]
struct Label {
    #[serde(rename = "type")]
    claim_type: ClaimType,
    quote: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq)]
struct Counts {
    tp: u32,
    fp: u32,
    fn_: u32,
}

impl Counts {
    fn precision(self) -> f64 {
        if self.tp + self.fp == 0 {
            1.0
        } else {
            f64::from(self.tp) / f64::from(self.tp + self.fp)
        }
    }
    fn recall(self) -> f64 {
        if self.tp + self.fn_ == 0 {
            1.0
        } else {
            f64::from(self.tp) / f64::from(self.tp + self.fn_)
        }
    }
    fn f1(self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Report {
    label: String,
    cases: usize,
    turns: usize,
    labelled_claims: usize,
    /// rule → counts
    rules: BTreeMap<String, Counts>,
    claims_regex: Counts,
    /// None when no recorded responses exist for the current prompt version.
    claims_llm: Option<Counts>,
    llm_cost_usd: Option<f64>,
    hallucinated_quotes_passed: u32,
    /// metric name → value, the numbers the baseline gate compares.
    metrics: BTreeMap<String, f64>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_cases() -> Result<Vec<(String, Case)>, String> {
    let dir = repo().join("tests/evals/divergence/cases");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries {
        let p = e.map_err(|e| e.to_string())?.path();
        if p.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let case = Case::parse(&p.display().to_string(), &text).map_err(|e| e.to_string())?;
        let name = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push((name, case));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn load_turns() -> Result<Vec<LabelledTurn>, String> {
    let p = repo().join("tests/evals/claims/turns.jsonl");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|e| format!("{}:{}: {e}", p.display(), i + 1))
        })
        .collect()
}

fn score_rules(cases: &[(String, Case)]) -> BTreeMap<String, Counts> {
    let mut out: BTreeMap<String, Counts> = RULES
        .iter()
        .map(|(r, _)| ((*r).to_string(), Counts::default()))
        .collect();
    for (_, case) in cases {
        let events = case.events();
        let graph = wyrd_correlator::correlate(&events);
        let claims = claims::extract_all(&claims::assistant_turns(&events));
        let files = case.files();
        let fired: BTreeSet<String> = evaluate(
            &Input {
                events: &events,
                graph: &graph,
                claims: &claims,
                files: &files,
            },
            &case.config(),
        )
        .into_iter()
        .map(|d| d.rule_id)
        .collect();
        let expected = case.expected();
        for (rule, c) in &mut out {
            match (expected.contains(rule), fired.contains(rule)) {
                (true, true) => c.tp += 1,
                (false, true) => c.fp += 1,
                (true, false) => c.fn_ += 1,
                (false, false) => {}
            }
        }
    }
    out
}

/// Match predicted claims to labels: same type and overlapping quotes.
fn score_claims(turn: &LabelledTurn, predicted: &[ClaimExtracted]) -> Counts {
    let mut used = vec![false; turn.claims.len()];
    let mut c = Counts::default();
    for p in predicted {
        let hit = turn.claims.iter().enumerate().position(|(i, l)| {
            !used[i]
                && l.claim_type == p.claim_type
                && (l.quote.contains(&p.quote) || p.quote.contains(&l.quote))
        });
        match hit {
            Some(i) => {
                used[i] = true;
                c.tp += 1;
            }
            None => c.fp += 1,
        }
    }
    c.fn_ = u32::try_from(used.iter().filter(|u| !**u).count()).unwrap_or(u32::MAX);
    c
}

fn add(a: &mut Counts, b: Counts) {
    a.tp += b.tp;
    a.fp += b.fp;
    a.fn_ += b.fn_;
}

/// Replays recorded response bodies by turn id; records live ones.
struct Recordings {
    live: Option<llm::AnthropicHttp>,
    recorded: BTreeMap<String, String>,
    current: std::cell::RefCell<String>,
    new: std::cell::RefCell<BTreeMap<String, String>>,
}

impl Transport for Recordings {
    fn send(&self, body: &Value) -> Result<Reply, String> {
        let id = self.current.borrow().clone();
        if let Some(live) = &self.live {
            let r = live.send(body)?;
            if r.status == 200 {
                self.new.borrow_mut().insert(id, r.body.clone());
            }
            return Ok(r);
        }
        self.recorded
            .get(&id)
            .map(|b| Reply {
                status: 200,
                body: b.clone(),
            })
            .ok_or_else(|| format!("no recorded response for {id}"))
    }
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let label = args
        .iter()
        .position(|a| a == "--label")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "dev".into());
    let write_baseline = args.iter().any(|a| a == "--write-baseline");

    let cases = load_cases()?;
    let turns = load_turns()?;
    let mut report = Report {
        label: label.clone(),
        cases: cases.len(),
        turns: turns.len(),
        labelled_claims: turns.iter().map(|t| t.claims.len()).sum(),
        rules: score_rules(&cases),
        ..Report::default()
    };

    score_turns(&mut report, &turns)?;
    fill_metrics(&mut report);

    let results = repo().join("docs/results");
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(results.join(format!("evals-{label}.json")), &json)
        .map_err(|e| e.to_string())?;
    std::fs::write(results.join(format!("evals-{label}.md")), markdown(&report))
        .map_err(|e| e.to_string())?;
    print!("{}", markdown(&report));

    let baseline_path = results.join("evals-baseline.json");
    if write_baseline {
        let m = serde_json::to_string_pretty(&report.metrics).map_err(|e| e.to_string())?;
        std::fs::write(&baseline_path, m + "\n").map_err(|e| e.to_string())?;
        println!("baseline written");
        return Ok(true);
    }
    gate(&report, &baseline_path)
}

/// Scores the claim extractors on the labelled turns; the LLM runs from
/// recorded responses, or live (and recording) with `EVAL_LIVE=1`.
fn score_turns(report: &mut Report, turns: &[LabelledTurn]) -> Result<(), String> {
    let rec_path = repo().join(format!(
        "tests/evals/claims/recorded/{}.json",
        llm::PROMPT_VERSION
    ));
    let live = std::env::var("EVAL_LIVE").is_ok_and(|v| v == "1");
    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if live && key.is_empty() {
        return Err("EVAL_LIVE=1 needs ANTHROPIC_API_KEY".into());
    }
    let recorded: BTreeMap<String, String> = std::fs::read_to_string(&rec_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let transport = Recordings {
        live: live.then(|| {
            llm::AnthropicHttp::new(
                "https://api.anthropic.com",
                key,
                std::time::Duration::from_secs(60),
            )
        }),
        recorded,
        current: std::cell::RefCell::new(String::new()),
        new: std::cell::RefCell::new(BTreeMap::new()),
    };
    let use_llm = live || !transport.recorded.is_empty();
    let mut llm_counts = Counts::default();
    let mut cost = 0.0;
    for t in turns {
        let turn = Turn {
            turn: 1,
            seq: 1,
            text: t.text.clone(),
        };
        let regex = claims::extract_regex(&turn);
        add(&mut report.claims_regex, score_claims(t, &regex));
        let mut all = regex.clone();
        if use_llm {
            transport.current.borrow_mut().clone_from(&t.id);
            let x = llm::extract(
                &transport,
                &llm::Settings::default(),
                std::slice::from_ref(&turn),
                &[],
                &|_| {},
            );
            cost += x.run.est_cost_usd;
            all = claims::merge(
                std::slice::from_ref(&turn),
                regex.into_iter().chain(x.claims),
            );
            add(&mut llm_counts, score_claims(t, &all));
        }
        report.hallucinated_quotes_passed +=
            u32::try_from(all.iter().filter(|c| !t.text.contains(&c.quote)).count())
                .unwrap_or(u32::MAX);
    }
    if use_llm {
        report.claims_llm = Some(llm_counts);
        report.llm_cost_usd = Some(cost);
    }
    if live {
        std::fs::create_dir_all(rec_path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        let json =
            serde_json::to_string_pretty(&*transport.new.borrow()).map_err(|e| e.to_string())?;
        std::fs::write(&rec_path, json).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn fill_metrics(report: &mut Report) {
    for (rule, c) in &report.rules {
        report
            .metrics
            .insert(format!("{rule}.precision"), c.precision());
        report.metrics.insert(format!("{rule}.recall"), c.recall());
    }
    report
        .metrics
        .insert("claims.regex.f1".into(), report.claims_regex.f1());
    if let Some(c) = report.claims_llm {
        report.metrics.insert("claims.llm.f1".into(), c.f1());
    }
    report.metrics.insert(
        "claims.no_hallucinated_quotes".into(),
        if report.hallucinated_quotes_passed == 0 {
            1.0
        } else {
            0.0
        },
    );
}

/// Compares the metrics with the committed baseline; false on any drop.
fn gate(report: &Report, baseline_path: &Path) -> Result<bool, String> {
    let text = std::fs::read_to_string(baseline_path)
        .map_err(|e| format!("{}: {e}", baseline_path.display()))?;
    let baseline: BTreeMap<String, f64> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", baseline_path.display()))?;
    let mut ok = true;
    for (name, base) in &baseline {
        match report.metrics.get(name) {
            Some(v) if *v + 1e-9 >= *base => {}
            Some(v) => {
                println!("REGRESSION {name}: {v:.4} < baseline {base:.4}");
                ok = false;
            }
            // The LLM metric is only present with recordings.
            None if name.starts_with("claims.llm") => {
                println!("note: {name} not measured (no recorded responses)");
            }
            None => {
                println!("MISSING {name}");
                ok = false;
            }
        }
    }
    Ok(ok)
}

fn markdown(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Divergence and claim evaluation — {}\n", r.label);
    let _ = writeln!(
        s,
        "`make test-evals` ({} labelled divergence cases, {} labelled assistant turns with {} claims). \
         The datasets are synthetic and were written together with the rules and patterns, so these \
         numbers are an upper bound for unseen agents.\n",
        r.cases, r.turns, r.labelled_claims
    );
    let _ = writeln!(
        s,
        "## Rules (target: precision ≥ 0.90, recall ≥ 0.85 per rule)\n"
    );
    let _ = writeln!(
        s,
        "| Rule | Name | TP | FP | FN | Precision | Recall |\n|---|---|---|---|---|---|---|"
    );
    for (rule, c) in &r.rules {
        let name = wyrd_divergence::rule_name(rule).unwrap_or("");
        let _ = writeln!(
            s,
            "| {rule} | {name} | {} | {} | {} | {:.3} | {:.3} |",
            c.tp,
            c.fp,
            c.fn_,
            c.precision(),
            c.recall()
        );
    }
    let _ = writeln!(
        s,
        "\n## Claims (target: F1 ≥ 0.90 with the LLM; 0 hallucinated quotes)\n"
    );
    let _ = writeln!(
        s,
        "| Extractor | TP | FP | FN | Precision | Recall | F1 |\n|---|---|---|---|---|---|---|"
    );
    let row = |s: &mut String, name: &str, c: Counts| {
        let _ = writeln!(
            s,
            "| {name} | {} | {} | {} | {:.3} | {:.3} | {:.3} |",
            c.tp,
            c.fp,
            c.fn_,
            c.precision(),
            c.recall(),
            c.f1()
        );
    };
    row(&mut s, "regex", r.claims_regex);
    match r.claims_llm {
        Some(c) => row(&mut s, &format!("regex + LLM ({})", llm::PROMPT_VERSION), c),
        None => {
            let _ = writeln!(
                s,
                "| regex + LLM | not measured: no recorded responses (`EVAL_LIVE=1 make test-evals` records them) | | | | | |"
            );
        }
    }
    if let Some(cost) = r.llm_cost_usd {
        let _ = writeln!(s, "\nLLM cost for the run: ${cost:.4}.");
    }
    let _ = writeln!(
        s,
        "\nHallucinated quotes passed through: **{}**.",
        r.hallucinated_quotes_passed
    );
    s
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("wyrd-evals: {e}");
            ExitCode::from(2)
        }
    }
}
