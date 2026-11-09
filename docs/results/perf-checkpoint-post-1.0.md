# Checkpoint latency after 1.0 — #239

This note records the post-1.0 work on the checkpoint p95 target (150 ms,
plan §18.2). It does not replace [perf-v1.0.md](perf-v1.0.md), which stays the
record of the v1.0 measurements. The target is **not met**: the change below
lowers the median and the p95 on ws11, and the p95 is still above 150 ms.

## Method

- Benchmark: `checkpoint_latency_p95_200_files_50mb_delta` in
  `crates/wyrd-cell/tests/perf.rs`, the test `make test-perf` runs. It is
  unchanged. Each round rewrites 200 files of 256 KiB in the upper layer and
  takes one checkpoint. Samples are taken from the test's own output.
- Machine: ws11 (48 hardware threads), shared and loaded during every run (load average 3 to 6, other
  users' jobs running). Release build of the `wyrd-cell` test binary.
- Comparison: the baseline and the changed binary run back to back, alternating
  which goes first in each pair, so run order does not favour either. Medians
  and p95 are over all samples of the same rounds. Fixed-order comparisons
  were not used: in four of five of them the second-run binary came out
  slower, including an identical binary (12 ms slower at the median).
- Differences under about 15 ms at the median are not treated as real here.
- Phase times: temporary timers in `Session::checkpoint` and `Cas::commit`
  (not committed).

## Where the time goes (baseline, 20 rounds, phase medians)

| Phase | Time | What it is |
|---|---|---|
| snapshot: walk, chunk, hash, compress, write staging files | ~19 ms | 8 workers, about 770 chunks |
| commit barrier 1 (`syncfs`) | ~55–60 ms | writeback of the staged chunks, and of the upper-layer data the workload just wrote (`syncfs` flushes the whole filesystem) |
| commit renames | ~12–15 ms | 770 renames, one thread |
| commit barrier 2 (`syncfs`) | ~16–40 ms | journal commit of the new names |
| ledger append (`synchronous=FULL`) | ~18–28 ms | one WAL commit with a log sync |
| ledger close | ~9 ms | last connection's WAL checkpoint (the benchmark opens and closes the ledger per checkpoint) |
| state save (`session.json`) | ~17 ms | `sync_data`, rename, directory sync |

About 115 ms of the roughly 155 ms median is a sync to disk. Each one is
required by the order in `docs/cas-format.md`: chunks durable before their
names, names durable before the ledger event, event durable before
`session.json` names it. None was weakened.

## What changed

**Writeback hint on staged chunks** (`Cas::stage`). After each staged file is
written, `posix_fadvise(DONTNEED)` is called on it. On Linux this starts the
writeback of the file's dirty pages straight away, while the other files are
still being chunked, so barrier 1 has less left to write. It is a hint, not a
barrier: nothing waits for it, its error is ignored, and barrier 1 (`syncfs`)
is unchanged and still covers every write. The argument, and the
alternatives, are in [ADR-009](../adr/009-checkpoint-writeback-hint.md).

Results, same binary, alternating order:

| Setup (host 48 threads; the checkpoint uses up to 8 workers) | Phase | Baseline | With hint |
|---|---|---|---|
| full host, 4 pairs × 15 rounds (n = 60 each) | barrier 1 median | 57.6 ms | 28.4 ms |
| | snapshot median | 19.2 ms | 22.4 ms |
| | **checkpoint median / p95** | **165.6 / 201.3 ms** | **133.8 / 160.6 ms** |
| 2 CPUs via `taskset -c 0,1` (2 workers), 3 pairs × 15 rounds (n = 45 each) | barrier 1 median | 58.0 ms | 18.0 ms |
| | snapshot median | 59.9 ms | 70.5 ms |
| | **checkpoint median / p95** | **209.3 / 243.5 ms** | **162.3 / 204.2 ms** |
| full host, 6 pairs × 20 rounds, final build (n = 120 each) | **checkpoint median / p95** | **184.6 / 226.3 ms** | **153.4 / 206.6 ms** |
| | min / max | 141.5 / 301.8 ms | 109.1 / 266.6 ms |

The snapshot phase gets about 3 ms slower on the full host and about 10 ms
slower on 2 CPUs, because of the extra `fadvise` call inside the freeze. The
barrier gains are larger than that.

Side effects. The staged chunks drop out of the page cache, so two kinds of
read go to disk that would otherwise hit the cache:

- the first read of a new chunk soon after a checkpoint;
- a later checkpoint that changes a file whose chunks already exist: the
  existing chunks are read to verify them (`Cas::stage` calls `verified`).
  The benchmark does not see this, because its data is all new.

Rewind (`rewind_latency_p95_1gb_workspace`, the 1 GB rewind, 10 rounds,
default settings), alternating order, 2 pairs. The first round after the
checkpoint is included:

| Build | Samples (rounds 1 to 10) | p95 |
|---|---|---|
| baseline, pair 1 | 1.18, 1.10, 1.11, 1.13, 1.12, 1.11, 1.13, 1.15, 1.13, 1.12 s | 1.18 s |
| with hint, pair 1 | 1.03, 0.97, 0.96, 0.95, 0.94, 0.97, 0.99, 0.97, 1.00, 0.95 s | 1.03 s |
| baseline, pair 2 | 1.16, 1.08, 1.09, 1.10, 1.09, 1.11, 1.09, 1.11, 1.11, 1.12 s | 1.16 s |
| with hint, pair 2 | 1.04, 0.97, 0.97, 0.98, 0.96, 0.96, 0.96, 0.95, 0.97, 0.98 s | 1.04 s |

No cold penalty showed on the rewind: round 1 was not slower. The target is
2 s, and the rewind stays well inside it.

## What was tried and not kept

| Change | Measured | Outcome |
|---|---|---|
| Chunk with FastCDC over 8 MiB windows instead of `StreamCDC`, same cut points, with a pinning test | single thread, one 50 MB file: `StreamCDC` 48 ms (including the reads), in-memory `FastCDC` 17 ms. Snapshot phase, same binary, alternating: full host 19.3 → 19.7 ms; 2 CPUs 60.0 → 59.0 ms | not kept: no gain in the phase that is measured. A rewrite of the format-critical chunker without a measured gain is not worth its risk |
| Rename staged chunks in parallel (8 threads, between the same barriers) | renames 15 → 12 ms | not kept: under the noise floor of the totals, and it adds threads to every commit |
| zstd level 1 instead of 3 | snapshot 18.2 / 17.6 ms vs 18.9 / 18.8 ms | not kept: about 1 ms; it stores larger chunks for compressible content |
| Snapshot workers 4 / 8 / 16 | snapshot 33 / 19 / 15.5 ms | not changed: the cap of 8 is a policy for shared machines |
| `fdatasync` of each staged file in its worker, instead of barrier 1 | barrier 1 falls to 12–22 ms, but the snapshot rises to about 2 s (one journal commit per file, serialised) | rejected |

## Options not done (need a decision)

- **An extra early `syncfs` during the snapshot.** It would also start the
  writeback early, but `crates/wyrd-cell/tests/checkpoint_sync_guard.rs` pins
  exactly two barriers per checkpoint (#253). Changing that guard is a
  decision for the team, not a change to make here.
- **Keep the ledger connection open across checkpoints.** Saves the ~9 ms
  close in long-lived sessions. A one-shot `wyrd checkpoint` opens and closes
  its own connection, so it would not gain.
- **Lower zstd level, or store incompressible chunks raw.** About 1 ms of wall
  time on this benchmark (see above). Raw storage would need a format change.
- **Hint the upper-layer files the snapshot reads.** The same early writeback
  could apply to the workspace's dirty data, which barrier 1 also flushes. It
  would evict workspace files from the cache after a checkpoint, a visible
  cost to the agent, so it is not done here.

## Status

- Target: checkpoint p95 150 ms **not met**. Measured here: median 153 ms and
  p95 207 ms (full host, 120 samples, loaded box). The v1.0 record was median
  139.9 ms and p95 158.7 ms under a different load.
- The checkpoint sync guard (exactly two barriers, no per-file sync, the
  event before the record) is unchanged and passes.
- The hosted-runner figure stays report-only; the nightly strict run stays
  red until the target is met.
- Issue #239 stays open.
