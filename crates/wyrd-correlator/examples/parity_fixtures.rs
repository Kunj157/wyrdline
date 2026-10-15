//! Writes `<dir>/<seed>.events.json` and the native correlator's
//! `<dir>/<seed>.graph.json` for the native-vs-Wasm parity check.
#![allow(clippy::unwrap_used)]

fn main() {
    let dir = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/wasm-parity".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    for seed in 0..40 {
        let g = wyrd_correlator::synth::session(seed);
        std::fs::write(
            dir.join(format!("{seed}.events.json")),
            serde_json::to_string(&g.events).unwrap(),
        )
        .unwrap();
        let graph = wyrd_correlator::correlate(&g.events);
        std::fs::write(
            dir.join(format!("{seed}.graph.json")),
            serde_json::to_string(&graph).unwrap(),
        )
        .unwrap();
    }
}
