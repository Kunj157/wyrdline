# Performance at v1.0 — plan §18.2

Measured with the release tests that `make test-perf` runs (filter
`throughput|overhead|latency`), one run each unless a count is given. Every
number below is copied from the test output of those runs. Earlier sensor
numbers and the design history are in [perf-sensor.md](perf-sensor.md) and
[studio-perf.md](studio-perf.md).

## Machine and method

- **Machine**: ws11, AMD EPYC 7443P (24 cores, 48 threads), 62 GB RAM,
  Ubuntu 24.04, kernel 6.17.0-14-generic. **The box is shared and loaded**:
  other users' sessions ran during every measurement. The load average was
  4–7 during the Cell runs, and 2–10 during the sensor runs. Numbers under
  load are noisy, so the sensor overhead tests compare interleaved runs
  (baseline, loaded, captured) and report medians. The sensor suite ran
  twice: on the committed revision (`bec0bd9`, run 2) and on the earlier
  code, which differs only in the victim-shop wrapper and the paced test
  (run 1). Both are reported.
- **Build**: `cargo nextest run --release --locked` (the same profile as
  `make test-perf`), with `wyrd-sensord/privileged-tests` for the sensor
  tests. Sensor tests run as the user; the daemon starts through `sudo -n`.
- **Timing harness**: the tests in `crates/wyrd-cell/tests/perf.rs` and
  `crates/wyrd-sensord/tests/perf.rs`. Percentiles are nearest-rank.

## Results

| Target (plan §18.2) | Measured | Result |
|---|---|---|
| Event loss at 50 k events/s | 200 000 opens paced at 50 k/s: **49 863 events/s** (run 2), 200 000 received, **0 dropped** (run 1: 49 862/s, 0 dropped) | **met** |
| Event throughput ceiling (no pacing) | 200 000 opens: 216 439 events/s (run 2), 223 310 events/s (run 1), 0 dropped | context |
| Checkpoint p95, 200 files, 50 MB delta | **p95 124 ms** over 3 runs × 20 rounds (per run: 109.6, 124.1, 118.1 ms); median 99.8 ms, min 80.8 ms, max 151.4 ms. Before the change, on the same harness: p95 7.22 s, median 5.52 s (20 rounds) | **met** (target ≤ 150 ms), see the note below |
| Rewind p95, 1 GB workspace | **p95 1.16 s**, median 0.93 s, 10 rounds (min 0.91 s, max 1.16 s) | **met** (target ≤ 2 s) |
| Denied-connect latency, p99 | **2.3–2.6 µs** (p99 per round, 3 rounds × 20 000 connects, both runs) | **met**. See the note below on what this compares to |
| Allowed-connect latency added, p99 | **+0.2 µs** (run 2), **+1.4 µs** (run 1) over a captured, not-enforced baseline (p99 ≈ 22–23 µs) | **met** (≤ 50 µs) |
| Overhead on `cargo build` (captured) | 5 rounds, median. Run 2: baseline 8.43 s; loaded, not registered 8.58 s (**+1.7 %**); captured 8.44 s (**+0.1 %**). Run 1: baseline 8.55 s; loaded −0.6 %; captured **−1.3 %** | **met** (≤ 5 %) |
| Overhead on `pnpm test` workload (captured) | victim-shop test script, 5 rounds, median. Run 2: baseline 117.60 ms; loaded +2.5 %; captured **−0.5 %**. Run 1: baseline 120.14 ms; loaded +0.9 %; captured **−0.2 %** | **met**, but the workload is about 120 ms, too short to trust, see follow-up |
| Judge cost per session | not measured: needs live `ANTHROPIC_API_KEY` | **not measured**, see #212 |

Context, not a target:

- **Syscall-dense stress** (about 7 000 effects per run, 9 rounds, median):
  run 2: baseline 433 ms; loaded, not registered +3.1 %; captured **+9.4 %**.
  Run 1: +3.6 % and **+9.7 %**. The captured figure is above the 5 % budget,
  which the plan defines for build and test workloads. It stays inside the
  12 % regression bound. See [perf-sensor.md](perf-sensor.md) for the design
  history.
- **Denied-connect note.** The denied p99 (2.3–2.6 µs) is faster than the
  captured baseline (p99 ≈ 22–23 µs). The two connects are not like for
  like: a denied connect is refused before the TCP path runs, and the
  baseline connects to loopback. The 50 µs bound is therefore applied to the
  allowed connect (above), and the denied figure is reported as measured.

## Missed targets and follow-ups

- **Checkpoint: fixed, target met on this box.** The cost was one fsync per
  changed chunk: 758 chunks per checkpoint, about 6 ms each, which is 97 %
  of the time. The chunking took 0.04 s. Chunks and the manifest are now
  written without a per-file barrier, changed files are stored by up to
  eight workers, and one `syncfs` before the ledger records the checkpoint
  makes them durable. The barrier is after the thaw, so the agent is not
  frozen for it. Measured on ws11 under other users' load (load average
  about 2). The run-2 maximum (151 ms) is just above the target. `syncfs`
  is filesystem-wide, so its time depends on other writers' dirty pages.
  See `docs/cas-format.md` and #239.
- **The `pnpm test` overhead is measured on a 120 ms workload.** The harness
  runs `node --test` directly (not `pnpm test`, so pnpm startup is not
  counted), and the demo suite fails one test on purpose, so the wrapper
  ignores node's exit status. A workload this short is dominated by noise.
  Follow-up: #240.
- **Wall-clock measurement is not done with hyperfine.** The issue names
  hyperfine. The harness uses interleaved medians instead, which is stricter
  on a shared box, but hyperfine is not installed. Follow-up: #240.
- **Judge cost per session is not measured.** It needs live API runs with an
  `ANTHROPIC_API_KEY`, which ws11 does not have. Tracked in #212.

## Reproduce

```
make test-perf          # release build, needs passwordless sudo; cleans the release target afterwards
```

Runs are logged in the test output (`--no-capture`). Each sample is printed
by the test that produced it, so medians and percentiles can be recomputed.
