# Enforcement latency — Phase 9

Target (plan §18.2): a denied connect adds ≤ 50 µs at p99.

Measured with `make test-perf` (`latency_of_connect_enforcement`, release
build) on the workstation (Ubuntu 24.04, kernel 6.17, load average ~2,
2026-10-19). A C program in a captured session scope times 20 000
non-blocking `connect()` calls with `CLOCK_MONOTONIC`; three rounds, the
median round reported. The enforced scope's allow-list holds loopback plus
1 000 host prefixes.

| Session | Destination | p50 | p99 |
|---|---|---|---|
| captured, not enforced | 127.0.0.1:1 (refused) | 14.8 µs | 23.6 µs |
| captured, enforced, allowed | 127.0.0.1:1 (refused) | 15.3 µs | 23.9 µs |
| captured, enforced, denied | 10.255.255.1:1 (`EPERM`) | 2.2 µs | 2.8 µs |

- **Allowed connects: +0.3 µs at p99** (one cgroup lookup and one LPM-trie
  lookup).
- **Denied connects: 2.8 µs at p99 end to end**, less than an allowed
  connect: the program refuses the call before the TCP stack does any
  work, so a denial adds nothing over a normal connect attempt.

All rounds:

```
round 0: not enforced p50 14.7 p99 23.1 µs; allowed p50 15.2 p99 23.0 µs; denied p50 2.2 p99 2.9 µs
round 1: not enforced p50 15.0 p99 24.1 µs; allowed p50 15.3 p99 25.1 µs; denied p50 2.2 p99 2.8 µs
round 2: not enforced p50 14.8 p99 23.6 µs; allowed p50 15.3 p99 23.9 µs; denied p50 2.2 p99 2.8 µs
```

## Escape suite

`make test-escape` (`tests/security/escape`): an agent in a Cell under the
`escape` fixture policy, with the sensor running. On the workstation
(2026-10-19) every attempt was refused:

| Attempt | Outcome |
|---|---|
| write to `$HOME` | denied (Landlock) |
| read `~/.ssh/id_test` | denied (Landlock) |
| read it through a symlink to the file / to the directory | denied |
| write to `$HOME` through a symlink | denied |
| write through `/proc/self/fd` of a read-only descriptor | denied |
| connect by IP outside the allow-list | denied (`EPERM`, cgroup program) |
| connect over IPv6 | denied |
| connect to an IPv4-mapped IPv6 address | denied |
| connect to a port outside the allow-list | denied (`EACCES`, Landlock) |
| an `LD_PRELOAD` library writing to `$HOME` and connecting | both denied |
| a renamed copy of `curl` | detected by content hash, agent stopped |
| an allowed host moving to another address | old address denied after the update (sensor test) |

Policy guests that loop forever or allocate past the memory cap are
denied by the host (`wyrd-policy` tests).
