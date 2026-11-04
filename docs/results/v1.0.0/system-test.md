# Full system test for v1.0.0 — plan §23

Status: **in progress. v1.0.0 is not declared.** Steps below that were not run
are listed as not run, not as passed.

Run on 2026-11-04 against `dev` at `b307ba2` with a debug build. This repeats
the 2026-11-03 run on `c44f584` in full, as plan §24 requires, because #266
updated the policy runtime (wasmtime 49.0.5) and #267 changed rewind and fork. Machine: ws11, Ubuntu 24.04, kernel 6.17.0-14-generic, shared and
loaded. The sensor ran as a throwaway `wyrd-sensord` through `sudo -n` on a
temporary socket, as the privileged tests do; no system unit was installed.
The agent is the scripted fakeagent running
`demo/scenarios/injection.wyrdscript.toml` under the default policy, with both
policy questions answered "allow", as in the demo.

## Summary

| Step | What                   | Result                                                        |
| ---- | ---------------------- | ------------------------------------------------------------- |
| 23.1 | Prepare                | Not green: see below; unit and `make hub-up` not run          |
| 23.2 | Baseline overhead      | Not run with hyperfine (#240); see the perf report            |
| 23.3 | Scripted demo          | **Pass**                                                      |
| 23.4 | Real agent             | Not run: needs the maintainer                                 |
| 23.5 | Rewind, fork, apply    | **Pass** with the fakeagent; two limits below                 |
| 23.6 | Tamper                 | **Pass** locally; Hub half covered by CI only                 |
| 23.7 | Offline, collaboration | **Pass** for export and scan; browser half covered by CI only |
| 23.8 | Backtest               | **Pass**                                                      |
| 23.9 | CI                     | Partial: gates green; Action in a test repo not run           |
| 23.10 | Observability         | Not run: Hub dashboards need Docker                           |
| 23.11 | Release evidence      | Partial: this file and `perf-v1.0.md` so far                  |

## Suites

| Target                  | Result                      |
| ----------------------- | --------------------------- |
| `make demo`             | 1 of 1 passed               |
| `make test-integration` | 27 of 27 passed (1 leaky)   |
| `make test-escape`      | 2 of 2 passed               |
| `make test-scenarios`   | 4 of 4 passed               |
| `-p wyrd-bundle -p wyrd-policy` | 64 of 64 passed, corpus too |

The leaky test is `privileged_drops_are_counted_with_a_tiny_ring_buffer`
(sensord golden), the same one as in the earlier runs.

The Cell's write set contains the real temporary directory, so these suites
fail if `TMPDIR` points somewhere else. Three tests failed that way in a first
attempt on 2026-11-03, with `Permission denied` from inside the Cell. That was the test setup
and the confinement working, not a defect: all three pass with `TMPDIR` unset.

## 23.1 Prepare

The plan asks for a green doctor; on this machine it is not. Kernel, BTF, ring
buffer, cgroup v2, delegated cgroup, Landlock ABI v7, overlayfs and the
socket-address hooks are fine. BPF LSM is off (optional). AppArmor restricts
unprivileged user namespaces, and there is no sensor at the system socket, so
plain `wyrd doctor` reports `Cell mode: no`. The runs below used
`WYRD_PRIVILEGED_HELPER=sudo` and a temporary sensor socket instead, and the
session records `mode cell`.

`make sensord-install`, installing `deploy/apparmor/wyrd` and `make hub-up`
change the system or need local containers. None of them is allowed on this
shared box, so none ran.

## 23.3 Scripted demo

- Divergences: exactly D001, D002, D003 and D006.
- Kernel denials: 1, the connect to `203.0.113.50:80`, recorded as denied.
- Checkpoints: 9 for 9 finished tool calls.
- Chain: intact, and `wyrd verify` still passes once sealed.
- Drops: `drops_since_last` is 0 on all 286 events of the session.

## 23.5 Rewind, fork, apply

`wyrd rewind <session> c1 --relaunch` printed:

```text
wyrd: now on branch b (from c1)
wyrd: warning: 1 effect after c1 cannot be undone:
  seq 50  net.connect 203.0.113.50:80 (denied)
```

The `checkpoint.rewound` event records `irreversible_seqs: [50]`. The agent
finished on branch `b` with 9 more checkpoints. `wyrd fork <session> c1 --name
side` then left branch `b` and printed the same warning (#267):

```text
wyrd: now on branch side (from c1)
wyrd: warning: 1 effect after c1 cannot be undone:
  seq 192  net.connect 203.0.113.50:80 (denied)
```

It lists only the connect on the branch being left. The one on branch `a` was
listed by the rewind that left it. `wyrd apply <session> b` then wrote the real
workspace, and all 6 files in branch `b`'s upper directory compare byte-equal
to the real workspace. The workspace held no other changes to compare.

Read this result with its limits:

- `wyrd diff <session> a b` reports 0 changes, because the scripted agent makes
  the same edits again after the relaunch. In a pass on 2026-11-03, with `b`
  rewound and not relaunched, the diff listed 10 changes. A relaunch
  that really diverges needs the real agent (§23.4).
- **A fork from Studio or the daemon API does not warn.** The warning is in the
  CLI, and `state.checkpoint.forked` does not record the list.
- **After `wyrd apply`, the dry run lists every applied file as a conflict.**
  Conflicts are measured against the session's baseline, so files that already
  equal the target count as changed by the user, and repeating the apply needs
  `--force`. No data is at risk, but the message is misleading (#265).

## 23.6 Tamper

On a copy of the sealed ledger, with its append-only trigger dropped, one byte
of `event_cbor` at seq 60 was flipped: `wyrd verify` exits 3 with `event
modified at seq 60`. Without dropping the trigger SQLite refuses the update
(`events are append-only`). The Hub half was not run here;
`crates/wyrd-hub/tests/containers_bundles.rs` rejects a tampered bundle in the
`Hub · Postgres and MinIO integration` job, green on this revision.

## 23.7 Offline replay and collaboration

A strict export verifies (structure, signature, manifest, file hashes, chain)
and imports read-only. Of the 24 unpacked bundle files, none contains the
planted key, the home path or the host name. The files are readable as stored:
the same search finds the connect address in them, so an empty result means
something. Export writes the home directory as `~`. The redaction corpus in
`crates/wyrd-bundle/tests/redaction.rs` passes. Offline Studio and two-browser
annotation sync with offline merge were not run by hand; they are covered by
`tests/e2e/offline.spec.ts`, `open-offline.spec.ts` and
`hub/annotations-hub.spec.ts` in the `Studio · end to end` job.

## 23.8 Backtest

Over the whole session, which holds the first run and the relaunch, each with
the same findings:

- `strict`: 12 findings, 6 per run (hook denials for the `.env` read and for curl, effect
  denials for the `.env` reads, kernel denials for curl and for the connect).
- `default`: 6 findings, 3 per run (2 asks and the connect denial).

## 23.9 CI on this revision

All 10 PR gate jobs are green on the head of #267 (`86bde6f`), including
`Cell · privileged integration`, `Hub · Postgres and MinIO integration` and
`Studio · end to end`. No nightly has run on `b307ba2` yet.

The last 5 nightly runs were on `c44f584`, before #266 and #267. The kernels
5.15 to 7.2 and both runner kernels are green there. `Sensor throughput and
overhead (strict)` failed in all 5. In the log of the 2026-11-03 run the only
failed test is the checkpoint target: p95 498 ms against 150 ms, the known miss
in #239. Release runs report this figure without gating on it (#252).

`cargo-deny` was green on `c44f584` when it merged and went red on the same tree
once RUSTSEC-2026-0422, 0423 and 0424 were published. #266 fixes that. Released
v0.13.1 still has wasmtime 49.0.4. From reading `wyrd-policy`, which was not
tested: 0422 and 0423 need host APIs it does not use (core-wasm `TypedFunc`, and
`StructType` across engines). 0424 applies in principle: a malicious policy
component could consume host memory, a denial of service.

The Action's PR comment and Hub upload in a real test repository were not run.

## Still open for v1.0.0

- §23.4 and the 5-minute video: the real agent, by the maintainer.
- §23.1 and §23.10: need a machine where installing the unit and the AppArmor
  profile and running local containers is allowed.
- §23.2: hyperfine (#240).
- §23.9: the Action in a test repository.
