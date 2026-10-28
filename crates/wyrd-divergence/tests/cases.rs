//! Every labelled case under `tests/evals/divergence/cases/` fires exactly
//! its expected rules, with evidence that exists in the session.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::Path;

use wyrd_divergence::case::Case;
use wyrd_divergence::{Input, claims, evaluate};

fn cases() -> Vec<(String, Case)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/evals/divergence/cases");
    let mut out: Vec<(String, Case)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let case = Case::parse(&p.display().to_string(), &text).unwrap();
            (name, case)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn every_case_fires_exactly_its_rules() {
    let mut failures = Vec::new();
    for (name, case) in cases() {
        let events = case.events();
        let graph = wyrd_correlator::correlate(&events);
        let claims = claims::extract_all(&claims::assistant_turns(&events));
        let files = case.files();
        let found = evaluate(
            &Input {
                events: &events,
                graph: &graph,
                claims: &claims,
                files: &files,
            },
            &case.config(),
        );
        let fired: BTreeSet<String> = found.iter().map(|d| d.rule_id.clone()).collect();
        if fired != case.expected() {
            failures.push(format!(
                "{name}: expected {:?}, fired {:?}\n{:#?}",
                case.expected(),
                fired,
                found
                    .iter()
                    .map(|d| (&d.rule_id, &d.summary, &d.evidence_seqs))
                    .collect::<Vec<_>>()
            ));
        }
        for d in &found {
            assert!(
                !d.evidence_seqs.is_empty(),
                "{name}: {} cites no evidence",
                d.rule_id
            );
            for s in &d.evidence_seqs {
                assert!(
                    events.iter().any(|e| e.seq == *s),
                    "{name}: {} cites missing seq {s}",
                    d.rule_id
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_rule_has_five_positives_and_three_hard_negatives() {
    let all = cases();
    for (rule, _) in wyrd_divergence::RULES {
        let prefix = rule.to_lowercase();
        let pos = all
            .iter()
            .filter(|(n, c)| {
                n.starts_with(&format!("{prefix}-pos")) && c.expect.iter().any(|r| r == rule)
            })
            .count();
        let neg = all
            .iter()
            .filter(|(n, c)| {
                n.starts_with(&format!("{prefix}-neg")) && !c.expect.iter().any(|r| r == rule)
            })
            .count();
        assert!(pos >= 5, "{rule}: {pos} positives");
        assert!(neg >= 3, "{rule}: {neg} hard negatives");
    }
}

#[test]
fn evaluation_is_deterministic() {
    for (_, case) in cases().into_iter().take(20) {
        let events = case.events();
        let graph = wyrd_correlator::correlate(&events);
        let (_, a) =
            wyrd_divergence::evaluate_rules_only(&events, &graph, &case.files(), &case.config());
        let (_, b) =
            wyrd_divergence::evaluate_rules_only(&events, &graph, &case.files(), &case.config());
        assert_eq!(a, b);
    }
}
