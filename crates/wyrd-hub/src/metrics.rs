//! Prometheus metrics (`GET /metrics`, text exposition format 0.0.4).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::Duration;

/// Upper bounds (seconds) of every latency histogram.
const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Every metric family the Hub exports: name, type, help.
const FAMILIES: &[(&str, &str, &str)] = &[
    (
        "wyrd_hub_http_requests_total",
        "counter",
        "HTTP requests by method, route, and status class.",
    ),
    (
        "wyrd_hub_http_request_duration_seconds",
        "histogram",
        "HTTP request latency by route.",
    ),
    (
        "wyrd_hub_db_pool_connections",
        "gauge",
        "Database pool connections by state.",
    ),
    (
        "wyrd_hub_logins_total",
        "counter",
        "OIDC logins by outcome.",
    ),
    (
        "wyrd_hub_uploads_total",
        "counter",
        "Bundle uploads by outcome.",
    ),
    (
        "wyrd_hub_bundle_verify_seconds",
        "histogram",
        "Time to verify an uploaded bundle.",
    ),
    (
        "wyrd_hub_retention_sweeps_total",
        "counter",
        "Retention sweeps run.",
    ),
    (
        "wyrd_hub_ci_token_exchanges_total",
        "counter",
        "GitHub Actions OIDC token exchanges by outcome.",
    ),
    (
        "wyrd_hub_ready",
        "gauge",
        "1 when the database is connected and migrated.",
    ),
    (
        "wyrd_hub_sync_connections_total",
        "counter",
        "Annotation sync connections accepted, by role.",
    ),
    (
        "wyrd_hub_sync_messages_total",
        "counter",
        "Annotation sync messages by direction and outcome (ok, read_only, too_large, invalid).",
    ),
    (
        "wyrd_hub_sync_clients",
        "gauge",
        "Annotation sync clients connected right now.",
    ),
    (
        "wyrd_hub_sync_documents",
        "gauge",
        "Annotation documents open in the relay.",
    ),
    (
        "wyrd_hub_sync_document_bytes",
        "gauge",
        "Changes held by the largest open annotation document, in bytes.",
    ),
];

type Labels = Vec<(&'static str, String)>;

#[derive(Debug, Default, Clone)]
struct Histogram {
    counts: [u64; BUCKETS.len()],
    count: u64,
    sum: f64,
}

/// Counters and histograms, keyed by family name and label set.
#[derive(Debug, Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<(&'static str, Labels), u64>>,
    histograms: Mutex<BTreeMap<(&'static str, Labels), Histogram>>,
}

/// Gauges read at scrape time.
#[derive(Debug, Default, Clone, Copy)]
pub struct Gauges {
    pub ready: bool,
    pub db_idle: u64,
    pub db_busy: u64,
    pub sync: wyrd_sync::SyncMetrics,
}

fn labels(pairs: &[(&'static str, &str)]) -> Labels {
    pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
}

impl Metrics {
    /// Add one to counter `name` with `pairs` as labels.
    pub fn inc(&self, name: &'static str, pairs: &[(&'static str, &str)]) {
        if let Ok(mut c) = self.counters.lock() {
            *c.entry((name, labels(pairs))).or_default() += 1;
        }
    }

    /// Record one observation of histogram `name`.
    pub fn observe(&self, name: &'static str, pairs: &[(&'static str, &str)], took: Duration) {
        let secs = took.as_secs_f64();
        if let Ok(mut h) = self.histograms.lock() {
            let h = h.entry((name, labels(pairs))).or_default();
            for (i, le) in BUCKETS.iter().enumerate() {
                if secs <= *le {
                    h.counts[i] += 1;
                }
            }
            h.count += 1;
            h.sum += secs;
        }
    }

    /// The current value of a counter (tests and the readiness log).
    pub fn counter(&self, name: &str, pairs: &[(&'static str, &str)]) -> u64 {
        let want = labels(pairs);
        self.counters.lock().map_or(0, |c| {
            c.iter()
                .find(|((n, l), _)| *n == name && *l == want)
                .map_or(0, |(_, v)| *v)
        })
    }

    /// Prometheus text exposition.
    pub fn render(&self, gauges: Gauges) -> String {
        let mut out = String::new();
        let counters = self.counters.lock().map(|c| c.clone()).unwrap_or_default();
        let histograms = self
            .histograms
            .lock()
            .map(|h| h.clone())
            .unwrap_or_default();
        for (name, kind, help) in FAMILIES {
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
            match *name {
                "wyrd_hub_ready" => {
                    let _ = writeln!(out, "{name} {}", u8::from(gauges.ready));
                }
                "wyrd_hub_db_pool_connections" => {
                    let _ = writeln!(out, "{name}{{state=\"idle\"}} {}", gauges.db_idle);
                    let _ = writeln!(out, "{name}{{state=\"busy\"}} {}", gauges.db_busy);
                }
                "wyrd_hub_sync_clients" => {
                    let _ = writeln!(out, "{name} {}", gauges.sync.connected);
                }
                "wyrd_hub_sync_documents" => {
                    let _ = writeln!(out, "{name} {}", gauges.sync.documents);
                }
                "wyrd_hub_sync_document_bytes" => {
                    let _ = writeln!(out, "{name} {}", gauges.sync.largest_document_bytes);
                }
                _ => {}
            }
            for ((n, l), v) in counters.iter().filter(|((n, _), _)| n == name) {
                let _ = writeln!(out, "{n}{} {v}", fmt_labels(l, None));
            }
            for ((n, l), h) in histograms.iter().filter(|((n, _), _)| n == name) {
                for (i, le) in BUCKETS.iter().enumerate() {
                    let _ = writeln!(
                        out,
                        "{n}_bucket{} {}",
                        fmt_labels(l, Some(&le.to_string())),
                        h.counts[i]
                    );
                }
                let _ = writeln!(out, "{n}_bucket{} {}", fmt_labels(l, Some("+Inf")), h.count);
                let _ = writeln!(out, "{n}_sum{} {}", fmt_labels(l, None), h.sum);
                let _ = writeln!(out, "{n}_count{} {}", fmt_labels(l, None), h.count);
            }
        }
        out
    }
}

/// `{k="v",...}` with values escaped; `le` appended for histogram buckets.
fn fmt_labels(l: &Labels, le: Option<&str>) -> String {
    let mut parts: Vec<String> = l
        .iter()
        .map(|(k, v)| {
            let v = v
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!("{k}=\"{v}\"")
        })
        .collect();
    if let Some(le) = le {
        parts.push(format!("le=\"{le}\""));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_histograms_and_gauges() {
        let m = Metrics::default();
        let l = [("method", "GET"), ("route", "/healthz"), ("class", "2xx")];
        m.inc("wyrd_hub_http_requests_total", &l);
        m.inc("wyrd_hub_http_requests_total", &l);
        m.observe(
            "wyrd_hub_http_request_duration_seconds",
            &[("route", "/healthz")],
            Duration::from_millis(20),
        );
        assert_eq!(m.counter("wyrd_hub_http_requests_total", &l), 2);
        let text = m.render(Gauges {
            ready: true,
            db_idle: 3,
            db_busy: 1,
            sync: wyrd_sync::SyncMetrics {
                connected: 2,
                largest_document_bytes: 512,
                ..wyrd_sync::SyncMetrics::default()
            },
        });
        assert!(text.contains(
            "wyrd_hub_http_requests_total{method=\"GET\",route=\"/healthz\",class=\"2xx\"} 2"
        ));
        assert!(text.contains(
            "wyrd_hub_http_request_duration_seconds_bucket{route=\"/healthz\",le=\"0.01\"} 0"
        ));
        assert!(text.contains(
            "wyrd_hub_http_request_duration_seconds_bucket{route=\"/healthz\",le=\"0.025\"} 1"
        ));
        assert!(
            text.contains("wyrd_hub_http_request_duration_seconds_count{route=\"/healthz\"} 1")
        );
        assert!(text.contains("wyrd_hub_ready 1"));
        assert!(text.contains("wyrd_hub_db_pool_connections{state=\"busy\"} 1"));
        assert!(text.contains("wyrd_hub_sync_clients 2"));
        assert!(text.contains("wyrd_hub_sync_document_bytes 512"));
    }

    #[test]
    fn label_values_are_escaped() {
        let m = Metrics::default();
        m.inc("wyrd_hub_http_requests_total", &[("route", "a\"b\\c\nd")]);
        let text = m.render(Gauges::default());
        assert!(text.contains(r#"route="a\"b\\c\nd""#), "{text}");
    }
}
