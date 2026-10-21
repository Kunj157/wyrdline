# The kernel sensor

`wyrd-sensord` is the only privileged Wyrdline process. It owns the eBPF
programs and streams each session's kernel effects to the user who owns
that session's cgroup.

## Programs (`crates/wyrd-ebpf`)

| Effect | Syscalls / tracepoints | Reported |
|---|---|---|
| exec | `execve`, `execveat` | on success, with filename, first 8 argv (64 B each) |
| process creation | `task:task_newtask` | new processes only (`CLONE_THREAD` filtered) |
| exit | `sched:sched_process_exit` + `exit_group` | group exit with exit code (−1 when killed) |
| open | `openat`, `openat2`, `open`, `creat` | on success; writes always, reads only for secret paths |
| unlink / rmdir | `unlinkat`, `unlink`, `rmdir` | on success |
| rename | `renameat2`, `renameat`, `rename` | on success |
| mkdir | `mkdirat`, `mkdir` | on success |
| chmod | `fchmodat`, `fchmodat2`, `chmod` | on success |
| truncate | `truncate` | on success |
| chdir | `chdir`, `fchdir` | used only to resolve later relative paths |
| connect | `connect` | every IPv4/IPv6 attempt |

- **Filtering:** every program first checks the task's cgroup (and up to 15
  ancestors) against the `SESSIONS` map; nothing else leaves the kernel.
- **Syscalls through the raw tracepoints:** one program on `raw_tp/sys_enter`
  dispatches by syscall number through the `SYSCALLS` map, which the
  loader fills for the host architecture (x86_64, aarch64); arguments are
  read from the saved `pt_regs` at per-architecture offsets. The
  per-syscall `syscalls:sys_enter_*` tracepoints are not used: on 7.0
  kernels the perf path behind them ran the programs for only a fraction
  of the path-taking calls (execve, mkdir, renameat2, unlinkat, openat),
  without any drop or miss being counted (#178). ia32 compat syscalls on
  x86_64 are skipped (their numbers come from another table); AArch32
  tasks on aarch64 are not told apart.
- **Layouts from the running kernel:** the two scheduler tracepoints
  whose layout changed (`task_newtask`, `sched_process_exit`) get their
  field offsets from `.rodata`, which the loader fills from tracefs
  `format` files. No struct offsets are compiled in, so no CO-RE
  relocations are needed.
- **Success only:** file operations, exec, and chdir are parked in a
  per-thread map at entry and emitted by `raw_tp/sys_exit` when the return
  value is ≥ 0.
- **Loss is visible:** `bpf_ringbuf_output` failures and syscalls that
  could not be parked increment a per-CPU counter, and program runs the
  kernel skipped (`recursion_misses` in the programs' fdinfo) are added to
  it; the daemon sends the total every second, sessions record it in
  `ringbuf_drops_since_last` and `wyrd run` warns.
- **Identity:** processes are `(tgid, start_ns)`; `start_ns` is recorded at
  process creation in a map, so PID reuse cannot merge two processes.

## Userspace (`crates/wyrd-sensor`)

The decoder completes events: relative paths are resolved against each
process's working directory, followed through `chdir`/`fchdir` and
inherited at fork (falling back to `/proc`, then the session workspace),
and `*at(dirfd, …)` paths against the directory descriptors the process
opened. Paths beneath the session workspace are marked `in_cell`.

## Daemon protocol

JSON lines over `/run/wyrdline/sensord.sock` (0666; authorisation is per
request):

```
→ {"type":"register","cgroup":"/sys/fs/cgroup/…/wyrd-<id>-r1.scope","workspace":"/home/u/repo"}
← {"type":"registered","cgroup_id":12345,"attached":6,"skipped":[]}
← {"type":"event","ts_mono_ns":…,"process":{…},"body":{"kind":"effect.file.rename","payload":{…}}}
← {"type":"drops","total":0}
```

- The cgroup must be a cgroup v2 directory owned by the caller's uid
  (`SO_PEERCRED`); root may register any.
- `registered` is sent only after the cgroup is in the kernel map, so a
  client may release its workload immediately without losing events.
- Closing the connection unregisters the cgroup.

## Deployment

`sudo make sensord-install` installs `/usr/local/bin/wyrd-sensord` and
`deploy/systemd/wyrd-sensord.service` (bounded capabilities, no network,
read-only system). Tests start their own daemon through `sudo -n` on a
temporary socket with `--exit-on-stdin-eof`.

## Egress enforcement

With a policy that lists egress rules (`wyrd run --policy`), `wyrd run`
sends them with the registration: the CIDRs, the addresses the allowed
host names resolve to, and the allowed ports. `wyrd-sensord` attaches
the `cgroup/connect4` and `cgroup/connect6` programs to that session's
cgroup only (a `bpf_link`, coexisting with other programs) and fills
per-session entries in an LPM trie (IPv4 and IPv6) and a port set. A
connect to anything else fails with `EPERM` in the kernel. IPv4-mapped
IPv6 destinations (`::ffff:a.b.c.d`) are checked against the IPv4 list.

The connect programs only decide; the `connect` syscall tracepoints report
every attempt at syscall exit with its verdict (in an enforced session,
`EPERM` comes from the program), so the ledger has
`effect.net.connect` with `verdict: denied` and `wyrd run` adds a
kernel-stage `guard.decision` next to it.

Host names are resolved by `wyrd run` with the system resolver, again
every 60 s; addresses a name no longer resolves to are removed from the
allow-list. Rules are IP-based: a CDN address shared with other names is
allowed for all of them (documented limitation). Without `wyrd-sensord`
the session is Degraded with "the network policy is not enforced in the
kernel"; tool-call checks still apply.
