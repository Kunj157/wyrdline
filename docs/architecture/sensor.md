# The kernel sensor

`wyrd-sensord` is the only privileged Wyrdline process. It owns the eBPF
programs and streams each session's kernel effects to the user who owns
that session's cgroup.

## Programs (`crates/wyrd-ebpf`)

| Effect | Tracepoints | Reported |
|---|---|---|
| exec | `sys_enter_execve(at)` → `sys_exit_*` | on success, with filename, first 8 argv (64 B each) |
| process creation | `task:task_newtask` | new processes only (`CLONE_THREAD` filtered) |
| exit | `sched:sched_process_exit` + `sys_enter_exit_group` | group exit with exit code (−1 when killed) |
| open | `openat`, `openat2`, `open`, `creat` | on success; writes always, reads only for secret paths |
| unlink / rmdir | `unlinkat`, `unlink`, `rmdir` | on success |
| rename | `renameat2`, `renameat`, `rename` | on success |
| mkdir | `mkdirat`, `mkdir` | on success |
| chmod | `fchmodat`, `fchmodat2`, `chmod` | on success |
| truncate | `truncate` | on success |
| chdir | `chdir`, `fchdir` | used only to resolve later relative paths |
| connect | `sys_enter_connect` | every IPv4/IPv6 attempt |

- **Filtering:** every program first checks the task's cgroup (and up to 15
  ancestors) against the `SESSIONS` map; nothing else leaves the kernel.
- **Stable layouts only:** syscall tracepoint arguments sit at
  `16 + 8·i` on every kernel. The two scheduler tracepoints whose layout
  changed (`task_newtask`, `sched_process_exit`) get their field offsets
  from `.rodata`, which the loader fills from tracefs `format` files. No
  struct offsets are compiled in, so no CO-RE relocations are needed.
- **Success only:** file operations, exec, and chdir are parked in a
  per-thread map at entry and emitted by one `sys_exit_generic` program
  (attached to 20 exit tracepoints) when the return value is ≥ 0.
- **Loss is visible:** `bpf_ringbuf_output` failures increment a per-CPU
  counter; the daemon sends the total every second and sessions record it.
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
← {"type":"registered","cgroup_id":12345,"attached":44,"skipped":[]}
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
