#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::range_plus_one,
    clippy::doc_markdown
)]

//! Attribution evaluation (plan §18). Sessions are generated from a seeded
//! RNG with ground truth known by construction, then scored:
//!
//! - precision = correct attributions / all attributions (call_id = Some)
//! - recall    = correct attributions / effects with a true cause
//!
//! Effects that are ambiguous by construction (inside two overlapping
//! windows with no direct link) are excluded from both and must come out
//! as `ambiguous`. Targets: precision ≥ 0.98, recall ≥ 0.95.

use std::collections::BTreeMap;

use wyrd_correlator::synth::{Gen, Truth, session};
use wyrd_correlator::{Attribution, correlate};

#[test]
fn attribution_precision_and_recall_meet_targets() {
    let (mut attributed, mut correct, mut with_cause, mut found) = (0u64, 0u64, 0u64, 0u64);
    let (mut amb_total, mut amb_flagged) = (0u64, 0u64);
    let mut by_kind: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let sessions = 300;
    let mut effects_total = 0u64;
    for seed in 0..sessions {
        let g = session(seed);
        let graph = correlate(&g.events);
        for (seq, truth) in &g.truth {
            let Some(e) = graph.effect(*seq) else {
                panic!("effect {seq} missing")
            };
            effects_total += 1;
            let entry = by_kind.entry(format!("{:?}", e.attribution)).or_default();
            entry.0 += 1;
            match truth {
                Truth::Ambiguous => {
                    amb_total += 1;
                    if e.attribution == Attribution::Ambiguous {
                        amb_flagged += 1;
                    }
                    continue;
                }
                Truth::Call(want) => {
                    with_cause += 1;
                    if e.call_id.as_deref() == Some(want.as_str()) {
                        found += 1;
                    }
                }
                Truth::None => {}
            }
            if let Some(got) = &e.call_id {
                attributed += 1;
                if matches!(truth, Truth::Call(w) if w == got) {
                    correct += 1;
                    entry.1 += 1;
                }
            }
        }
    }
    let precision = correct as f64 / attributed as f64;
    let recall = found as f64 / with_cause as f64;
    eprintln!(
        "attribution eval: {sessions} sessions, {effects_total} labelled effects; precision {precision:.4} ({correct}/{attributed}), \
         recall {recall:.4} ({found}/{with_cause}); ambiguous flagged {amb_flagged}/{amb_total}; by attribution {by_kind:?}"
    );
    assert!(precision >= 0.98, "precision {precision:.4}");
    assert!(recall >= 0.95, "recall {recall:.4}");
    assert_eq!(
        amb_flagged, amb_total,
        "ambiguous effects must never be silently attributed"
    );
}

#[test]
#[ignore = "throughput benchmark; make test-perf (release)"]
fn throughput_at_least_100k_events_per_second() {
    let sessions: Vec<Gen> = (0..200).map(session).collect();
    let n: usize = sessions.iter().map(|g| g.events.len()).sum();
    let start = std::time::Instant::now();
    for g in &sessions {
        std::hint::black_box(correlate(&g.events));
    }
    let rate = n as f64 / start.elapsed().as_secs_f64();
    eprintln!("correlator throughput: {n} events, {rate:.0} events/s");
    assert!(rate >= 100_000.0, "{rate:.0} events/s");
}
