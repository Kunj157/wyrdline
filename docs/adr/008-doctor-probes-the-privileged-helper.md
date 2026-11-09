# ADR-008: `wyrd doctor` probes the privileged helper with `sudo -n -l`

- Status: Accepted
- Date: 2026-11-09

## Context

On Ubuntu 23.10 and later, unprivileged user namespaces are restricted by
AppArmor. A user can still run the Cell by setting
`WYRD_PRIVILEGED_HELPER=sudo`: `wyrd run` then runs stage two as
`sudo -n -- wyrd __cell-exec <spec>` and drops back to the user before the
agent starts ([Cell](../architecture/cell.md)). `wyrd run` uses this path,
but `wyrd doctor` ignored it: `Cell mode` said `no` and the process exited 2
(issue #312).

Doctor cannot simply count the variable as "yes". Whether `sudo -n` will run
the helper without a password is a fact about sudoers, and the report must not
claim it without checking. This ADR also sets a rule for `wyrd-doctor`: it
reads files and takes probe results as inputs, and it never runs a command
itself. The sudo call happens in the caller, `wyrd-cell`.

## Decision

1. **The probe is `sudo -n -l -- <wyrd> __cell-exec <placeholder>`**, run only
   when `WYRD_PRIVILEGED_HELPER=sudo`. `sudo -l` with a command lists the
   matching sudoers rule and does not run the command. `-n` makes sudo fail
   instead of prompting. The placeholder spec path has the same argument
   shape as the real call, so a sudoers rule that matches the real call
   matches the probe. The probe is `wyrd_cell::launch::probe_sudo_helper`,
   and `wyrd-doctor` receives its result through `Probes`.
2. **Results are `PrivilegedHelper`:** `not_configured`, `verified`,
   `refused` (with sudo's own first stderr line, or the exit status), and
   `not_probed`. `not_probed` is for the daemon, which runs no sudo. It does
   not count toward Cell mode.
3. **Cell mode is available through the helper only when `verified`.** The
   rule: all required checks are OK, and one of userns, the helper, or
   `wyrd-sensord` holds. The report names the path it used (`cell_path`:
   `user_namespace`, `privileged_helper` or `sensord`), in the order
   `wyrd run` prefers the user-namespace and helper paths.
4. **The userns check is unchanged** (status and detail). It still reports
   the AppArmor restriction. The helper is a separate line in the report, so
   the restriction stays visible.
5. **Only `wyrd doctor` probes.** `wyrd run` reads only the kernel release
   and the capability list from its doctor report, so it does not probe, and
   the capability list it records in the ledger is unchanged. The daemon
   (`wyrdd`) reports `not_probed`, because it would otherwise have to run
   sudo for every `/system/doctor` request. The daemon avoids shelling out
   in general, and this per-request sudo call would be the least
   appropriate case for it.

## Consequences

- `wyrd doctor` prints `Cell mode: yes` and exits 0 when sudo allows the
  helper without a password, and adds a line
  `Privileged helper (WYRD_PRIVILEGED_HELPER=sudo): sudo -n allows the Cell's
  stage two without a password (Cell mode via the privileged helper)`. When
  the probe fails, the line says `refused:` with sudo's reason.
- On a host where the helper is the only Cell path, the daemon's
  `/system/doctor` answers `not_probed` and `cell_mode: false`. This is a
  known gap; see Decision 5 and `docs/limitations.md`.
- The report schema gains `cell_path` and `privileged_helper` (additive). No
  ledger, event, or bundle format changes.
- Each `wyrd doctor` with the helper enabled runs one `sudo -n -l`. It asks
  for no password and prints nothing.

### Limitation: the probe checks sudoers, not the run

`sudo -l` with a command reports whether sudo allows the command. It does
not start stage two, so it cannot show that the Cell's namespace setup
succeeds. The result is also a snapshot: a sudo timestamp or a later sudoers
edit can change what the next run does.

The residual risk is a false `verified`. Sudo's default `listpw=any` means
`-l` needs no password when some other entry is NOPASSWD. The code assumes
that `-n -l` with a command exits non-zero when the matching entry itself
needs a password. That is sudo's documented behaviour, but this host cannot
test it: its sudoers (`(ALL) NOPASSWD: ALL`) has no entry that requires a
password for `wyrd`, and changing sudoers is out of scope. The fake-`sudo`
tests in `crates/wyrd-cell/tests/helper_probe.rs` cover the exit-status
mapping, not sudo's matching.

The failure mode is safe. A false `verified` makes `wyrd run` fail at launch
with the sudo error, and the session is marked Failed. The launch code
(`choose_mode` in `Session::start`, `crates/wyrd-cell/src/session.rs`) has no
fallback to observe mode and no unconfined run. To check on a host where the
rule matters, run `sudo -n -l -- <wyrd> __cell-exec x`; a password
requirement must give a non-zero exit.

## Alternatives considered

- **`sudo -n true`.** It tests for any NOPASSWD rule, not the wyrd rule. A
  user with the documented minimal grant (NOPASSWD only for
  `wyrd __cell-exec` and `__cell-cleanup`) would get a false `refused`.
- **Dry run of `sudo -n wyrd __cell-exec`.** This runs wyrd as root from a
  doctor probe. Stage two has no do-nothing mode, so it would need one.
- **Count the variable as `verified`.** Makes the report claim something that
  was never checked. Rejected.
- **A new `CheckId` row.** It would add a key to `capabilities()`, which
  `wyrd run` writes to the ledger, and the daemon's copy would always say
  false. A report field keeps the answer in one place.
- **Probe in the daemon too.** Makes the daemon run sudo on every doctor
  request. Rejected for now; a daemon-side probe could be added later behind
  a cache.
