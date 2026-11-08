//! Per-session numbers for `/v1/metrics`, counted from the ledger: events by
//! plane, dropped events, sensor losses, denials, divergences, and
//! checkpoint durations. A [`Tally`] only ever grows by the events appended
//! after `head_seq`, so a scrape reads each event once.

use std::collections::BTreeMap;

use wyrd_divergence::llm::JudgeRun;
use wyrd_schema::{Decision, Event, EventBody, NetVerdict};

/// Upper bounds (ms) of the checkpoint duration histogram. The duration is
/// the one `state.checkpoint.created` records: snapshot and CAS commit, up to
/// but not including the ledger append and the session state save, so it is
/// below the whole-call time the 150 ms target (§18.2) is measured on.
pub const CHECKPOINT_BUCKETS_MS: [u64; 10] = [10, 25, 50, 100, 150, 250, 500, 1000, 2500, 5000];

/// Longest label value kept; longer or unusual values become `other`.
const MAX_LABEL: usize = 32;

/// Counts from one session's events.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    /// Last sequence number counted.
    pub head_seq: u64,
    /// Events by plane name.
    pub events: BTreeMap<&'static str, u64>,
    /// Sum of `drops_since_last`: effects the sensor lost.
    pub dropped: u64,
    pub sensor_lost: u64,
    /// Denied actions by event kind.
    pub denied: BTreeMap<&'static str, u64>,
    /// Divergences raised by rule id.
    pub divergences: BTreeMap<String, u64>,
    pub checkpoints: Histogram,
}

/// A cumulative-bucket histogram over [`CHECKPOINT_BUCKETS_MS`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Histogram {
    /// Observations at or below each bound (not cumulative; rendering sums).
    pub buckets: [u64; CHECKPOINT_BUCKETS_MS.len()],
    pub count: u64,
    pub sum_ms: u64,
}

impl Histogram {
    pub fn observe(&mut self, ms: u64) {
        if let Some(i) = CHECKPOINT_BUCKETS_MS.iter().position(|b| ms <= *b) {
            self.buckets[i] += 1;
        }
        self.count += 1;
        self.sum_ms = self.sum_ms.saturating_add(ms);
    }

    pub fn merge(&mut self, o: &Histogram) {
        for (a, b) in self.buckets.iter_mut().zip(o.buckets) {
            *a += b;
        }
        self.count += o.count;
        self.sum_ms = self.sum_ms.saturating_add(o.sum_ms);
    }
}

/// A rule id as a label value: short, `[A-Za-z0-9_.-]`, else `other`.
fn label(v: &str) -> String {
    let ok = !v.is_empty()
        && v.len() <= MAX_LABEL
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    if ok { v.to_string() } else { "other".into() }
}

impl Tally {
    /// Count one event. Events at or below `head_seq` were counted already
    /// and are ignored, so overlapping reads cannot double-count.
    pub fn add(&mut self, e: &Event) {
        if e.seq <= self.head_seq {
            return;
        }
        self.head_seq = e.seq;
        *self.events.entry(e.plane().as_str()).or_default() += 1;
        self.dropped += u64::from(e.drops_since_last);
        let denied = match &e.body {
            EventBody::SensorLost(_) => {
                self.sensor_lost += 1;
                false
            }
            EventBody::NetConnect(n) => n.verdict == NetVerdict::Denied,
            EventBody::ToolCallDenied(_) => true,
            EventBody::GuardDecision(g) => g.decision == Decision::Deny,
            EventBody::DivergenceRaised(d) => {
                *self.divergences.entry(label(&d.rule_id)).or_default() += 1;
                false
            }
            EventBody::CheckpointCreated(c) => {
                self.checkpoints.observe(u64::from(c.duration_ms));
                false
            }
            _ => false,
        };
        if denied {
            *self.denied.entry(e.body.kind()).or_default() += 1;
        }
    }

    /// Add another session's counts (`head_seq` is per session and not
    /// summed).
    pub fn merge(&mut self, o: &Tally) {
        for (k, v) in &o.events {
            *self.events.entry(k).or_default() += v;
        }
        self.dropped += o.dropped;
        self.sensor_lost += o.sensor_lost;
        for (k, v) in &o.denied {
            *self.denied.entry(k).or_default() += v;
        }
        for (k, v) in &o.divergences {
            *self.divergences.entry(k.clone()).or_default() += v;
        }
        self.checkpoints.merge(&o.checkpoints);
    }
}

/// Token usage and estimated cost of the LLM claim extractor.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Judge {
    pub runs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
}

impl Judge {
    /// Add the runs in a `judge-runs.jsonl` file; lines that do not parse
    /// are skipped, as `wyrd verdict` skips them.
    pub fn add_lines(&mut self, text: &str) {
        for run in text
            .lines()
            .filter_map(|l| serde_json::from_str::<JudgeRun>(l).ok())
        {
            self.runs += 1;
            self.input_tokens += run.usage.input_tokens;
            self.output_tokens += run.usage.output_tokens;
            self.cache_creation_input_tokens += run.usage.cache_creation_input_tokens;
            self.cache_read_input_tokens += run.usage.cache_read_input_tokens;
            if run.est_cost_usd.is_finite() && run.est_cost_usd > 0.0 {
                self.cost_usd += run.est_cost_usd;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrd_schema::{
        CheckpointCreated, DivergenceRaised, GuardDecision, GuardStage, NetConnect, Severity,
        ToolCallDenied,
    };

    fn ev(seq: u64, drops: u32, body: EventBody) -> Event {
        Event {
            seq,
            ts_mono_ns: seq,
            ts_wall_ms: 0,
            process: None,
            drops_since_last: drops,
            body,
        }
    }

    fn connect(verdict: NetVerdict) -> EventBody {
        EventBody::NetConnect(NetConnect {
            family: 4,
            addr: "203.0.113.50".into(),
            port: 80,
            dns_name: None,
            verdict,
        })
    }

    fn denied_call() -> EventBody {
        EventBody::ToolCallDenied(ToolCallDenied {
            call_id: "c1".into(),
            policy_id: "default".into(),
            reason: "no".into(),
        })
    }

    fn guard(decision: Decision) -> EventBody {
        EventBody::GuardDecision(GuardDecision {
            stage: GuardStage::Kernel,
            decision,
            policy_id: "default".into(),
            policy_version: "1".into(),
            reason: "r".into(),
            call_id: None,
            subject_seq: None,
        })
    }

    fn divergence(rule: &str) -> EventBody {
        EventBody::DivergenceRaised(DivergenceRaised {
            divergence_id: format!("d-{rule}"),
            rule_id: rule.into(),
            severity: Severity::High,
            claim_id: None,
            evidence_seqs: vec![1],
            confidence_bp: 9000,
            summary: "s".into(),
        })
    }

    fn checkpoint(ms: u32) -> EventBody {
        EventBody::CheckpointCreated(CheckpointCreated {
            checkpoint_id: "c".into(),
            branch_id: "a".into(),
            parent_id: None,
            after_call_id: None,
            manifest: "b3:00".into(),
            files_changed: 1,
            bytes_added: 1,
            duration_ms: ms,
        })
    }

    #[test]
    fn counts_each_event_once() {
        let session = wyrd_correlator::synth::session(1).events;
        let mut t = Tally::default();
        for e in &session {
            t.add(e);
        }
        assert_eq!(t.events.values().sum::<u64>(), session.len() as u64);
        assert_eq!(t.head_seq, session.last().unwrap().seq);
        let total = t.clone();
        // Already counted: a second pass changes nothing.
        for e in &session {
            t.add(e);
        }
        assert_eq!(t, total);
    }

    #[test]
    fn each_kind_lands_in_its_counter() {
        let mut t = Tally::default();
        t.add(&ev(1, 0, connect(NetVerdict::Denied)));
        t.add(&ev(2, 3, connect(NetVerdict::Allowed)));
        t.add(&ev(3, 0, denied_call()));
        t.add(&ev(4, 0, guard(Decision::Deny)));
        t.add(&ev(5, 0, guard(Decision::Allow)));
        t.add(&ev(6, 0, divergence("D003")));
        t.add(&ev(7, 0, divergence("D003")));
        t.add(&ev(8, 0, divergence("bad rule\n")));
        for (seq, ms) in [(9, 5), (10, 150), (11, 151), (12, 9000)] {
            t.add(&ev(seq, 0, checkpoint(ms)));
        }
        assert_eq!(t.dropped, 3);
        assert_eq!(t.denied["effect.net.connect"], 1);
        assert_eq!(t.denied["intent.tool_call.denied"], 1);
        assert_eq!(t.denied["guard.decision"], 1);
        assert_eq!(t.denied.len(), 3);
        assert_eq!(t.divergences["D003"], 2);
        assert_eq!(t.divergences["other"], 1);
        let h = &t.checkpoints;
        assert_eq!((h.count, h.sum_ms), (4, 5 + 150 + 151 + 9000));
        assert_eq!(h.buckets[0], 1, "5 ms is in the 10 ms bucket");
        assert_eq!(h.buckets[4], 1, "150 ms is at the target bound");
        assert_eq!(h.buckets[5], 1, "151 ms is above it");
        assert_eq!(h.buckets.iter().sum::<u64>(), 3, "9 s is only in +Inf");
    }

    #[test]
    fn merge_adds_sessions() {
        let mut a = Tally::default();
        a.add(&ev(1, 2, denied_call()));
        let mut sum = Tally::default();
        sum.merge(&a);
        sum.merge(&a);
        assert_eq!(sum.dropped, 4);
        assert_eq!(sum.denied["intent.tool_call.denied"], 2);
        assert_eq!(sum.events["intent"], 2);
    }

    #[test]
    fn judge_runs_sum_and_skip_bad_lines() {
        let mut j = Judge::default();
        j.add_lines(concat!(
            r#"{"model":"m","prompt_version":"v1","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":7},"est_cost_usd":0.0015,"latency_ms":50,"attempts":1,"dropped":0,"error":null}"#,
            "\nnot json\n",
            r#"{"model":"m","prompt_version":"v1","usage":{"input_tokens":1},"est_cost_usd":-1.0,"latency_ms":1,"attempts":1,"dropped":0,"error":"timeout"}"#,
        ));
        assert_eq!(j.runs, 2);
        assert_eq!((j.input_tokens, j.output_tokens), (101, 20));
        assert_eq!(j.cache_read_input_tokens, 7);
        assert!((j.cost_usd - 0.0015).abs() < 1e-12, "{}", j.cost_usd);
    }
}
