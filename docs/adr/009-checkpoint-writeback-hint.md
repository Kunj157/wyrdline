# ADR-009: Writeback hint on staged chunks in a checkpoint

- Status: Accepted
- Date: 2026-11-09

## Context

A checkpoint writes its changed chunks to staging names, then commits them
with two `syncfs` barriers and renames in between (`docs/cas-format.md`,
"Durability"). The first barrier writes back the staged data and was about
55 to 60 ms of a checkpoint of 200 files and 50 MB (#239). The target is a
p95 of 150 ms, and the order of the barriers is what makes a checkpoint
crash-consistent (#245, #285), so the barriers themselves cannot change.

Linux starts writeback of a file's dirty pages when asked. `posix_fadvise` with
`POSIX_FADV_DONTNEED` does that: it starts writeback of the dirty pages without
waiting for it, then drops the clean ones. `rustix` exposes it safely, and the
crate forbids `unsafe`, so no other writeback call is available here.

## Decision

Right after each staged chunk is written, call `posix_fadvise(DONTNEED)` on
it, and ignore the result. The commit is otherwise unchanged.

## Why the order still holds

- **Nothing is made durable by the hint.** It only starts writeback. Whether
  the data is on disk is decided by the commit's barrier 1, `syncfs`, which
  waits for every write made so far to the filesystem, including the pages the
  hint started writing. Barrier 1 still runs before the first rename, and
  barrier 2 still runs after the renames, before the ledger event.
- **No barrier is removed, added, or moved.** The sync guard
  (`crates/wyrd-cell/tests/checkpoint_sync_guard.rs`) still sees exactly two
  `syncfs` barriers per checkpoint and no per-file sync.
- **The hint is not a step.** It is not recorded by `record_steps`, so the
  step order the tests pin does not include it. A missing hint changes timing
  only.
- **A failed hint is harmless.** Its error is ignored, and the same data is
  written and synced by the commit either way.

## Consequences

- Barrier 1 writes less at the end of the checkpoint: 57.6 to 28.4 ms at the
  median on the full host, 58.0 to 18.0 ms with two CPUs (see
  `docs/results/perf-checkpoint-post-1.0.md`).
- The snapshot is slower by about 3 ms on the full host and about 10 ms with
  two CPUs, because of the extra call inside the freeze.
- Staged chunks drop out of the page cache. Two kinds of read now go to disk
  that would have come from the cache:
  - the first read of a new chunk soon after the checkpoint, such as a rewind
    right after it (rewind p95 on ws11 stayed 1.03 to 1.04 s, against 1.16 to
    1.18 s before, so this did not show there);
  - a checkpoint that changes a file whose chunks already exist, which reads
    those chunks to verify them (`Cas::stage` calls `verified`).
  These are reads, not extra writes, and the benchmark does not measure them,
  because its data is all new.
- Keeping the pages cached would need a writeback primitive that
  `rustix` does not expose; a `sync_file_range` call would need `unsafe`. If
  that is wanted later, it is a new decision.

## Alternatives considered

- **An extra early `syncfs` during the snapshot.** It would start the same
  writeback without evicting pages, but the sync guard pins exactly two
  barriers per checkpoint (#253), so this changes the design the guard
  records. Left for a decision.
- **`fdatasync` of each staged file in its worker.** Barrier 1 falls to
  12 to 22 ms, but each file is a separate journal commit: the snapshot rises
  to about 2 s. Rejected.
- **Lower zstd level, or parallel renames, or a windowed chunker.** Each was
  measured within the noise of the phase it changes. Not kept.
