# Full system test for v1.0.0 — plan §23

Status: **in progress. v1.0.0 is not declared.** Steps below that were not run
are listed as not run, not as passed.

Run on 2026-11-06 against `dev` at `a0fa527`, with a debug build. Plan §24
requires a full repeat because #268 changed the Cell's apply, and #276 and
#277 changed rewind and fork answers. Earlier runs: `c44f584` (2026-11-03) and
`b307ba2` (2026-11-04, after the policy-runtime update #266).

Machine: ws11, Ubuntu 24.04, kernel 6.17.0-14-generic, shared and loaded. The
sensor ran as a throwaway `wyrd-sensord` through `sudo -n` on a temporary
socket, as the privileged tests do; no system unit was installed. The agent is
the scripted fakeagent running `demo/scenarios/injection.wyrdscript.toml`
under the default policy, with both policy questions answered "allow", as in
the demo.

The local steps (§23.3, §23.5, and the local halves of §23.6 and §23.7, plus
§23.8) now run as `make system-test` (`scripts/system-test.sh`, #278). Every
step checks its result, and the script fails if any check fails. This run:
**30 passed, 0 failed**.

## Summary

| Step  | What                   | Result                                                        |
| ----- | ---------------------- | ------------------------------------------------------------- |
| 23.1  | Prepare                | Partial: unit and Hub readiness by CI; gaps below             |
| 23.2  | Baseline overhead      | Interleaved medians instead of hyperfine (decision, #270)     |
| 23.3  | Scripted demo          | **Pass**                                                      |
| 23.4  | Real agent             | Not run: needs the maintainer                                 |
| 23.5  | Rewind, fork, apply    | **Pass** with the fakeagent; limits below                     |
| 23.6  | Tamper                 | **Pass** locally; Hub half covered by CI only                 |
| 23.7  | Offline, collaboration | **Pass** for export and scan; browser half covered by CI only |
| 23.8  | Backtest               | **Pass**                                                      |
| 23.9  | CI                     | Partial: gates green; Action in a test repo not run           |
| 23.10 | Observability          | Not run: Hub dashboards need Docker                           |
| 23.11 | Release evidence       | Partial: this file and `perf-v1.0.md` so far                  |

## Suites

| Target                          | Result                      |
| ------------------------------- | --------------------------- |
| `make demo`                     | 1 of 1 passed               |
| `make test-integration`         | 27 of 27 passed (1 leaky)   |
| `make test-escape`              | 2 of 2 passed               |
| `make test-scenarios`           | 4 of 4 passed               |
| `-p wyrd-bundle -p wyrd-policy` | 64 of 64 passed, corpus too |
| `make system-test`              | 30 of 30 checks passed      |

The leaky test is `privileged_drops_are_counted_with_a_tiny_ring_buffer`
(sensord golden), the same one as in every earlier run.

The Cell's write set contains the real temporary directory, so these suites
fail if `TMPDIR` points somewhere else. Three tests failed that way in a first
attempt on 2026-11-03, with `Permission denied` from inside the Cell. That was
the test setup and the confinement working, not a defect.

## 23.1 Prepare

The plan asks for a green doctor; on this machine it is not. Kernel, BTF, ring
buffer, cgroup v2, delegated cgroup, Landlock ABI v7, overlayfs and the
socket-address hooks are fine. BPF LSM is off (optional). AppArmor restricts
unprivileged user namespaces, and there is no sensor at the system socket, so
plain `wyrd doctor` reports `Cell mode: no`. The runs used
`WYRD_PRIVILEGED_HELPER=sudo` and a temporary sensor socket instead, and the
session records `mode cell`.

`make sensord-install`, installing `deploy/apparmor/wyrd` and `make hub-up`
change the system or need local containers. None of them is allowed on this
shared box, so none ran here. By decision (2026-11-08), CI evidence stands in
for them where a CI job really does the step:

- **Doctor on a CI runner** (`Cell · privileged integration`, run
  42305266012 on `cc6c0cf`): kernel 7.0.0-1017-azure, BTF, ring buffer,
  cgroup v2, delegated user cgroup, Landlock ABI v8, overlayfs in user
  namespaces and the socket-address hooks pass; BPF LSM is off (optional);
  AppArmor restricts user namespaces and no system sensor runs, so it also
  reports `Cell mode: no`. The job runs the privileged suites through `sudo`,
  as here. The step does not gate (`|| true`).
- **The sensor unit** (`Smoke install`, release run 40975370164 for v0.13.1):
  installs the `.deb`, passes `systemd-analyze verify` on the unit, starts
  `wyrd-sensord`, checks that it is active and that
  `/run/wyrdline/sensord.sock` exists, then stops it and removes the package.
  `deploy/`, the installer and `wyrd-doctor` have not changed since v0.13.1.
- **A Hub that is ready** (`Chart · kind smoke`, run 40931523952 on
  `a4cccb3`; the Hub and its chart have not changed since): the Hub image
  with PostgreSQL 17 and MinIO in kind, installed by the Helm chart, answers
  `/readyz`.

Not covered by any CI job:

- **The AppArmor profile.** The `.deb` installs `/etc/apparmor.d/wyrd`, but no
  job loads it and runs a Cell without `sudo`, so the profile itself is
  untested.
- **`make hub-up`.** There is no such target and no compose file
  (plan §10.4); the Hub runs in CI only through Testcontainers and kind.

## 23.2 Baseline overhead

The plan names hyperfine. The overhead tests instead alternate baseline,
loaded and captured runs within each round and take medians, which controls
drift on a shared box better (decision recorded in `perf-v1.0.md`, #270). The
victim-shop workload now repeats the test suite 20 times per measurement. On
ws11: captured +0.3 % and +0.5 %. On the GitHub runner, in the 2026-11-07
nightly on `cc6c0cf`: loaded +1.0 %, captured +0.6 % (target ≤ 5 %; the
2026-11-06 nightly measured +2.1 % and +2.0 %).

## 23.3 Scripted demo

- Divergences: exactly D001, D002, D003 and D006.
- Kernel denials: 1, the connect to `203.0.113.50:80`, recorded as denied.
- Checkpoints: one per finished tool call (9 for 9).
- Chain: intact, and `wyrd verify` still passes once sealed.
- Drops: `drops_since_last` is 0 on all 286 events of the session.

## 23.5 Rewind, fork, apply

`wyrd rewind <session> c1 --relaunch` printed:

```text
wyrd: now on branch b (from c1)
wyrd: warning: 1 effect after c1 cannot be undone:
  seq 50  net.connect 203.0.113.50:80 (denied)
```

The agent finished on branch `b` with 9 more checkpoints. `wyrd fork <session>
c1 --name side` then left branch `b` and warned about the relaunched run's
connect:

```text
wyrd: now on branch side (from c1)
wyrd: warning: 1 effect after c1 cannot be undone:
  seq 192  net.connect 203.0.113.50:80 (denied)
```

It lists only the connect on the branch being left; the rewind that left
branch `a` listed the one there. `wyrd apply <session> b` then wrote the real
workspace, and all 6 files in branch `b`'s upper directory compare byte-equal
to it. A second `apply --dry-run` lists no conflicts (#268; before that, every
applied file counted as one).

The same list now reaches the other surfaces: the daemon's rewind and fork
answers carry `irreversible_seqs` (#276), and Studio's dialogs list the
effects before you confirm, by the same branch rule (#269, #277).

Read this result with its limits:

- `wyrd diff <session> a b` reports 0 changes, because the scripted agent makes
  the same edits again after the relaunch. In a pass on 2026-11-03, with `b`
  rewound and not relaunched, the diff listed 10 changes. A relaunch that
  really diverges needs the real agent (§23.4).
- A relaunch requested through the daemon (202) runs detached, so its answer
  has no list; the warning goes to `relaunch.log`.
- Studio counts allowed connections and writes outside the Cell; the CLI also
  lists denied connection attempts (`docs/limitations.md`).

## 23.6 Tamper

On a copy of the sealed ledger, with its append-only trigger dropped, one byte
of `event_cbor` at seq 60 was flipped: `wyrd verify` exits 3 with `event
modified at seq 60`. Without dropping the trigger SQLite refuses the update
(`events are append-only`). The Hub half was not run here;
`crates/wyrd-hub/tests/containers_bundles.rs` rejects a tampered bundle in the
`Hub · Postgres and MinIO integration` job.

## 23.7 Offline replay and collaboration

A strict export verifies (structure, signature, manifest, file hashes, chain)
and imports read-only. Of the 24 unpacked bundle files, none contains the
planted key, the home path or the host name. The files are readable as stored:
the same search finds the connect address in them, so an empty result means
something. The redaction corpus in `crates/wyrd-bundle/tests/redaction.rs`
passes. Offline Studio and two-browser annotation sync with offline merge were
not run by hand; `tests/e2e/offline.spec.ts`, `open-offline.spec.ts` and
`hub/annotations-hub.spec.ts` cover them in the `Studio · end to end` job.

## 23.8 Backtest

Over the whole session, which holds the first run and the relaunch, each with
the same findings:

- `strict`: 12 findings, 6 per run (hook denials for the `.env` read and for
  curl, effect denials for the `.env` reads, kernel denials for curl and for
  the connect).
- `default`: 6 findings, 3 per run (2 asks and the connect denial).

## 23.9 CI

The PR gate (10 jobs, including `Cell · privileged integration`, `Hub ·
Postgres and MinIO integration` and `Studio · end to end`) was green on every
PR merged into `a0fa527`.

The 2026-11-07 nightly ran on `cc6c0cf`, which differs from `a0fa527` only in
CI and documentation (#274, #279), so it counts for this run. All six VM
kernels (5.15, 6.1, 6.6, 6.12, 7.0, 7.2) and both runner kernels (Ubuntu 24.04
and 26.04) passed. That includes 5.15 and 6.1, whose QEMU install had stalled
in the 2026-11-06 nightly before #274 bounded and retried it. In `Sensor
throughput and overhead (strict)` 9 of 10 tests passed. The only failure is
the checkpoint target: p95 483 ms against 150 ms, the known miss in #239.
Release runs report it without gating (#252). The other figures from that
run: paced 49 767 events/s with 0 drops, captured overhead +1.2 % on
`cargo build` and +0.6 % on victim-shop, and +10.6 % on the syscall-dense
stress workload (context only).

`cargo-deny` went red twice on unchanged code when advisories were published:
wasmtime (RUSTSEC-2026-0422, 0423, 0424, fixed by #266) and the unmaintained
`astral-tokio-tar` behind testcontainers (RUSTSEC-2026-0440, accepted with a
reason in `deny.toml` by #271, test-only dependency). Released v0.13.1 still has
wasmtime 49.0.4. From reading `wyrd-policy`, which was not tested: 0422 and
0423 need host APIs it does not use, and 0424 applies in principle (a
malicious policy component could consume host memory, a denial of service).
By decision (2026-11-08) there is no v0.13.1 patch release: the wasmtime fix
ships in v1.0.0 and its release notes name it. The advisory ignore from #271
stays until testcontainers drops the crate.

The Action's PR comment and Hub upload in a real test repository were not run.

## Still open for v1.0.0

- §23.4 and the 5-minute video: the real agent, by the maintainer.
- §23.1: the AppArmor profile is untested and `make hub-up` does not exist
  (above). CI evidence covers the rest and does not block v1.0.0 (decision,
  2026-11-08).
- §23.9: the Action in a test repository.
- #239: checkpoint p95 above the 150 ms target (reported, not gating).
