#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::print_stderr
)]

//! Cell measurements (`make test-perf`, release build). Checkpoint and
//! rewind latency against the plan's v1.0 targets (§18.2):
//!
//! - checkpoint p95 for 200 changed files (≤ 50 MB delta): ≤ 150 ms;
//! - rewind p95 for a 1 GB workspace: ≤ 2 s.
//!
//! Each test prints every sample and the p95, then asserts the target.
//! Sizes can be lowered for a quick local check:
//! `WYRD_PERF_REWIND_MB=64 WYRD_PERF_ROUNDS=3 cargo nextest run ...`.
//! `WYRD_PERF_TMP` chooses where the temporary workspace is created
//! (default: the test target tmpdir); it is deleted when the test ends.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

fn scratch() -> tempfile::TempDir {
    match std::env::var_os("WYRD_PERF_TMP") {
        Some(root) => {
            fs::create_dir_all(&root).unwrap();
            tempfile::tempdir_in(root).unwrap()
        }
        None => tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap(),
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Deterministic, incompressible-looking bytes, different per `seed`.
fn payload(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// The nearest-rank 95th percentile: the `ceil(0.95 * n)`-th smallest sample.
fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    let idx = (v.len() * 95).div_ceil(100).max(1) - 1;
    v[idx]
}

fn session(data: &DataDir, ws: &Path) -> Session {
    Session::create(
        data,
        CreateOptions {
            workspace: ws.to_path_buf(),
            command: vec!["true".into()],
            observe: false,
            name: None,
            agent_kind: "perf".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "perf".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap()
}

/// Checkpoint latency: each round rewrites 200 files of 256 KiB (a 50 MB
/// delta) in the session's upper layer, then takes a checkpoint.
#[test]
#[ignore = "release measurement; make test-perf"]
fn checkpoint_latency_p95_200_files_50mb_delta() {
    const FILES: usize = 200;
    const FILE_LEN: usize = 256 * 1024;
    let rounds = env_usize("WYRD_PERF_ROUNDS", 20);
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = session(&data, &ws);
    let upper = s.dir().upper("a");

    let mut samples = Vec::with_capacity(rounds);
    for r in 0..rounds {
        for f in 0..FILES {
            let body = payload(FILE_LEN, (r * FILES + f) as u64 + 1);
            fs::write(upper.join(format!("f{f:03}.bin")), body).unwrap();
        }
        let t = Instant::now();
        s.checkpoint(None).unwrap();
        samples.push(t.elapsed());
    }
    let p = p95(samples.clone());
    eprintln!(
        "checkpoint: {FILES} files, {} MB delta, {rounds} rounds, samples {samples:?}, p95 {p:.2?}",
        FILES * FILE_LEN / (1024 * 1024)
    );
    assert!(
        p <= Duration::from_millis(150),
        "checkpoint p95 {p:?} exceeds 150 ms"
    );
}

/// Rewind latency: a 1 GB workspace (in the upper layer, so the checkpoint
/// holds all of it) is checkpointed; each round rewinds to that checkpoint
/// on a new branch, which materializes the full tree.
#[test]
#[ignore = "release measurement; make test-perf"]
fn rewind_latency_p95_1gb_workspace() {
    const FILE_MB: usize = 4;
    let mb = env_usize("WYRD_PERF_REWIND_MB", 1024);
    let rounds = env_usize("WYRD_PERF_ROUNDS", 10);
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = session(&data, &ws);
    let upper = s.dir().upper("a");
    let files = mb.div_ceil(FILE_MB);
    for d in 0..16 {
        fs::create_dir_all(upper.join(format!("d{d:03}"))).unwrap();
    }
    for f in 0..files {
        let path = upper.join(format!("d{:03}/f{f:04}.bin", f % 16));
        fs::write(path, payload(FILE_MB * 1024 * 1024, f as u64 + 7)).unwrap();
    }
    let base = s.checkpoint(None).unwrap();
    // A later checkpoint, so the rewind target is not the branch head.
    fs::write(upper.join("d000/after.txt"), "later\n").unwrap();
    s.checkpoint(None).unwrap();

    let mut samples = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let t = Instant::now();
        let branch = s.rewind(&base.id, None).unwrap();
        samples.push(t.elapsed());
        // Keep the disk bounded: each materialized branch is a full copy.
        fs::remove_dir_all(s.dir().branch(&branch.id)).unwrap();
    }
    let p = p95(samples.clone());
    eprintln!(
        "rewind: {files} files, {mb} MB workspace, {rounds} rounds, samples {samples:?}, p95 {p:.2?}"
    );
    assert!(p <= Duration::from_secs(2), "rewind p95 {p:?} exceeds 2 s");
}
