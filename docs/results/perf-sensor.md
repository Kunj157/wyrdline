# Sensor performance — Phase 5

Measured with `make test-perf` (release build). Two machines:

- **Workstation**: Ubuntu 24.04, kernel 6.17, 48 cores, shared. The design
  steps below were measured while it was heavily loaded (load average
  80–90); the 2026-10-16 runs at load average ~1.5.
- **Hosted runner**: the nightly `perf` job of `kernel-matrix.yml`
  (`ubuntu-24.04`, 4 cores), which enforces the bounds below.

## Overhead target (plan §18.2): ≤ 5 % on build and test workloads

Workload: debug `cargo build -p wyrd-correlator` with all its dependencies
(serde, syn, schemars, ciborium, ts-rs; offline, fresh target directory,
`-j 4`), 5 rounds, baseline / sensor loaded / captured interleaved, medians.
Asserted: captured ≤ 5 % and loaded-but-unregistered ≤ 5 %.

| Date | Machine | No sensor | Loaded, not registered | Captured | Effects per run |
|---|---|---|---|---|---|
| 2026-10-16 | workstation, load ~1.5 | 7.95 s | 7.96 s (**+0.2 %**) | 8.01 s (**+0.8 %**) | 1 880 |
| 2026-10-16 | hosted runner (run 38897030147) | 11.24 s | 11.37 s (**+1.2 %**) | 11.41 s (**+1.5 %**) | 1 880 |

The plan also names `pnpm test` of the demo repository; that repository
(`demo/victim-shop`) does not exist yet and is measured once it does.

## Worst case: syscall-dense workload (regression guard, not the target)

The build-like stress script below produces ~20 k effects/s, several times a
real build. Captured sessions cost **more than 5 %** here; the test holds
them to an explicit **12 % regression bound** so a slower kernel path fails
the nightly job, while the cost every *unregistered* process on the machine
pays stays asserted at ≤ 5 %.

| Date | Machine | Loaded, not registered | Captured |
|---|---|---|---|
| 2026-10-16 | hosted runner (nightly 38865958905) | +1.6 % | +9.1 % |
| 2026-10-16 | hosted runner (run 38897030147) | +1.3 % | +9.4 % |
| 2026-10-16 | workstation, load ~1.5 | +0.9 % | +5.9 % |

## Throughput and loss

| Run | Events | Time | Rate | Drops |
|---|---|---|---|---|
| final design | 200 000 file opens | 1.02 s | **196 692 events/s** | **0** |
| final design, machine under load 90 | 200 000 | 1.35 s | 148 677 events/s | 0 |

Target (plan §18.2): ≥ 50 k events/s with zero loss at a 64 MiB ring
buffer — met. Every one of the 200 000 events was received.

## Syscall-dense workload: design history

Workload: a build-like shell script (1 500 generated source files, `git
add`/`commit`, `tar czf`/`xzf`, `grep -r`, 150 process spawns; ~7 000
recorded effects per run, about 9–15 k effects/s — far denser than a
typical agent session).

| Design step | No sensor | Sensor loaded, process **not** registered | Process **captured** |
|---|---|---|---|
| initial (ancestor walk, every open parked, 1.2 KB copies, wakeup per event) | 708 ms | +7.5 % | +20.9 % |
| + depth bitmask, read-open filter in kernel, size-tiered output | 683 ms | +4.0 % | +16.7 % |
| + no wakeup per event (daemon drains every 10 ms) | 469 ms | **+3.4 %** | **+8.1 %** |
| tried: entry event + result record instead of parking | 736 ms | +1.9 % | +10.8 % (and 10 % drops at 200 k events/s → reverted) |
| final, rounds interleaved, load average 90 | 717 ms | +6.0 % | +9.2 % |

Reading:

- The cost every process on the machine pays when the sensor is loaded is
  within the 5 % budget in quiet runs (+3.4 %, +4.0 %); the +6.0 % run was
  taken at load average 90, where repeated runs of identical code differ
  by several percent.
- The cost for the captured session on this syscall-dense stress workload
  is +6–9 %, above 5 %. Remaining per-event work in the kernel is the
  parking of the event in a per-thread map between syscall entry and exit.
  On the build workload the plan's target is defined for, capture costs
  under 1 % (table above).

## What changed and why

1. **Depth bitmask** (`LEVELS`): programs return immediately when no
   session is registered and otherwise test one ancestor cgroup per depth
   that has sessions, instead of walking up to 16 ancestors per syscall.
2. **Read-only opens filtered in the kernel**: kept only for directories
   (needed to resolve later `*at(dirfd, …)` paths) and secret-looking paths
   (a dot component or `.pem`/`.key`/`.p12`).
3. **Size-tiered ring-buffer records**: header + one path for most kinds,
   two paths for renames, full record (argv) only for exec.
4. **No wakeup per record** (`BPF_RB_NO_WAKEUP`): the daemon drains every
   10 ms; at most 10 ms of extra latency.
