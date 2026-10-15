# Sensor performance — Phase 5

Measured with `make test-perf` (release build) on the development
workstation: Ubuntu 24.04, kernel 6.17, 48 cores, **shared and heavily
loaded during the runs (load average 80–90)**. Results are reproduced
nightly on a quiet GitHub runner by the `perf` job of `kernel-matrix.yml`,
which enforces the overhead bound (`WYRD_PERF_STRICT=1`).

## Throughput and loss

| Run | Events | Time | Rate | Drops |
|---|---|---|---|---|
| final design | 200 000 file opens | 1.02 s | **196 692 events/s** | **0** |
| final design, machine under load 90 | 200 000 | 1.35 s | 148 677 events/s | 0 |

Target (plan §18.2): ≥ 50 k events/s with zero loss at a 64 MiB ring
buffer — met. Every one of the 200 000 events was received.

## Overhead

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
  is +8–9 %, above the 5 % target. Remaining per-event work in the kernel
  is the parking of the event in a per-thread map between syscall entry
  and exit. **The ≤ 5 % exit criterion for captured sessions is not met on
  this workload yet**; the strict CI job reports the quiet-machine number.

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
