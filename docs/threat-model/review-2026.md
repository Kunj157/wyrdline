# 1.0 security review

Issue [#166](https://github.com/Kunj157/wyrdline/issues/166). Reviewed on
2026-10-28 against `origin/dev` (08bd466) and the heads of the open PRs
listed below. Each finding names the PR it depends on. Where a PR is still
open, the finding must be re-checked when that PR merges.

Threat model changes are in [scope.md](scope.md) (the "Boundaries added for
1.0" section). This file holds the findings, the `unsafe` report, the
Semgrep rules, the dependency review and the `wyrd-sensord` capability audit.

## Summary

| ID | Component | Severity | Depends on | Status |
|---|---|---|---|---|
| F-1 | `wyrd-sensord` keeps CAP_SYS_ADMIN, CAP_DAC_READ_SEARCH, CAP_SYS_RESOURCE after attach | medium | dev | issue [#226](https://github.com/Kunj157/wyrdline/issues/226) |
| F-2 | `wyrd-sensord` runs as root; no seccomp filter; comment and `ProtectHome` disagree | medium | dev | issue [#227](https://github.com/Kunj157/wyrdline/issues/227) |
| F-3 | `wyrdd` accepts a checkpoint, branch or target starting with `-`, passed as argv to the `wyrd` child | low | dev | fixed in this PR (`check_target` rejects a leading `-`, unit test) |
| F-4 | MCP rug-pull pins live in memory only; a restart re-pins | medium | dev (adapter) | issue [#222](https://github.com/Kunj157/wyrdline/issues/222) |
| F-5 | Agent-controlled text (rewind reason, tool summaries) is returned to the agent as history | medium | #209, #218 | issue [#223](https://github.com/Kunj157/wyrdline/issues/223) |
| F-6 | `wyrdd` spawns the `wyrd` binary (relaunch, Landlock probe) | low | dev | accepted (see below) |
| F-7 | `wyrd-bundle` `redact.rs` compiles constant patterns with `expect` | info | dev | accepted (see below) |
| F-8 | Action installs an unsigned release under `verify-signature: auto` | medium | dev | issue [#224](https://github.com/Kunj157/wyrdline/issues/224) |
| F-9 | Action installs the sensor with `sudo -n` on the runner | low | dev | accepted (see below) |
| F-10 | Owner-wide CI trust rules let any repository of an owner upload | medium | #216 | issue [#225](https://github.com/Kunj157/wyrdline/issues/225) |
| F-11 | `aya::Pod` impls in `wyrd-common` lack compile-time padding checks | low | dev | issue [#228](https://github.com/Kunj157/wyrdline/issues/228) |
| F-12 | `cargo-deny` advisories: four zstd RUSTSEC entries | high (red CI) | dev | fixed by #215, not this PR |

Checked and found sound (no finding): bundle reader rejects traversal,
links, devices and oversized or over-compressed entries before exposing
content; Hub upload body limit (`DefaultBodyLimit`, #216); OIDC state is
one-use, browser-bound and expiring (#216 base); CI token issuer and
audience checks (#216); Hub object keys are server-generated
(`bundles/{org}/{ulid}.wyrd`, the ULID is minted by the Hub and the org comes
from the caller's membership, #198/#216 `routes/bundles.rs`); every external `uses:` is pinned to a 40-character
SHA; no `curl | sh`; action inputs reach scripts through `env:` only; Helm
chart runs non-root with a read-only root filesystem and drops all
capabilities; the Dockerfile pins base images by digest (#210); the Studio
service worker caches only the app shell and never `v1/` or `bundle/` paths
(#214, enforced by its test); #217 changes only drop counting, not
privileges.

## Hardening applied in this PR

- `#![forbid(unsafe_code)]` in 21 crate root files of 19 crates that contain no
  `unsafe` (all workspace crates except `wyrd-common`, `wyrd-ebpf` and `wyrd-sensor`).
  Any future `unsafe` in those crates fails to compile.
- Semgrep rules `.semgrep/wyrd.yml`, run in the `Security` CI job (below).
- `check_target` rejects a leading `-` (F-3), with a unit test.

## `unsafe` report (cargo-geiger)

Method: `cargo geiger --output-format Ratio --all-features` from each crate
directory (cargo-geiger 0.14.0 needs a package, not the virtual workspace).
Shared build directory; stale dep-info from other branches was pruned first.
The tool reports `:)` for "no unsafe, forbids it", `?` for "no unsafe, does not
forbid it", and `!` for "unsafe used". Columns are used/total for functions,
expressions, impls, traits and methods.

| Crate | Symbol | Functions | Expressions | Impls | Traits | Methods |
|---|---|---|---|---|---|---|
| wyrd-adapters | `:)` | 60/60 | 1618/1618 | 8/8 | 3/3 | 35/35 |
| wyrd-bundle | `:)` | 78/78 | 2243/2243 | 11/11 | 1/1 | 21/21 |
| wyrd-cas | `:)` | 10/10 | 824/824 | 8/8 | 0/0 | 26/26 |
| wyrd-cell | `:)` | 51/51 | 2936/2936 | 11/11 | 0/0 | 92/92 |
| wyrd-cli | `:)` | 142/142 | 4482/4482 | 15/15 | 0/0 | 35/35 |
| wyrd-common | `!` | 0/0 | 7/7 | 1/4 | 0/0 | 0/0 |
| wyrd-correlator | `:)` | 12/12 | 1123/1123 | 4/4 | 0/0 | 19/19 |
| wyrd-divergence | `?` | 75/75 | 3427/3427 | 12/12 | 1/1 | 40/40 |
| wyrd-doctor | `:)` | 20/20 | 339/339 | 3/3 | 0/0 | 10/10 |
| wyrd-fakeagent | `:)` | 18/18 | 958/958 | 4/4 | 0/0 | 22/22 |
| wyrd-gateway | `:)` | 14/14 | 624/624 | 9/9 | 2/2 | 13/13 |
| wyrd-hub | `:)` | 131/131 | 3415/3415 | 20/20 | 0/0 | 58/58 |
| wyrd-ledger | `:)` | 21/21 | 759/759 | 5/5 | 0/0 | 25/25 |
| wyrd-otlp | `:)` | 10/10 | 165/165 | 0/0 | 0/0 | 0/0 |
| wyrd-policy | `:)` | 23/23 | 837/837 | 7/7 | 0/0 | 14/14 |
| wyrd-schema | `?` | 10/10 | 136/136 | 2/2 | 0/0 | 3/3 |
| wyrd-sensord | `:)` | 21/21 | 747/747 | 4/4 | 0/0 | 5/5 |
| wyrd-sync | `:)` | 11/11 | 596/596 | 11/11 | 1/1 | 30/30 |
| wyrd-wasm | `:)` | 14/14 | 373/373 | 2/2 | 0/0 | 11/11 |
| wyrdd | not produced | | | | | |
| wyrd-ebpf, wyrd-sensor | excluded by scope | | | | | |

`wyrdd`: cargo-geiger stopped on a path it could not read (`.../wyrdline-hub/the`)
and produced no row. A grep of `crates/wyrdd` finds no `unsafe` token outside
strings; re-run geiger when the tool is fixed. Tracked by the `unsafe`
note here, not an issue.

`wyrd-divergence` and `wyrd-schema` contain `#![forbid(unsafe_code)]` and geiger
still reports `?`; the symbol logic of the tool does not match that attribute
placement. The attribute is enforced by the compiler; this is a tool quirk.

### Every `unsafe` outside `wyrd-ebpf` and `wyrd-sensor`

There is one kind of `unsafe` in our own code outside the two excluded crates,
and it is in `wyrd-common`:

```
crates/wyrd-common/src/lib.rs   unsafe impl aya::Pod for super::RawEvent {}
                                unsafe impl aya::Pod for super::Offsets {}
                                unsafe impl aya::Pod for super::PortKey {}
```

They sit in `mod user`, behind `feature = "user"`, and are used only by the
user-space loader to read kernel-written records. Justification:

- `aya::Pod` promises that the type is plain data: every bit pattern is a
  valid value and reading it from a byte buffer is sound. All three are
  `#[repr(C)]` structs of integers and fixed-size integer arrays, so no field
  holds a reference, a pointer or an enum with invalid discriminants.
- `PortKey` has an explicit `_pad` field, so its padding is never implicit.
- `RawEvent` and `Offsets` have implicit padding if their fields are not
  ordered by size. The `SAFETY` comment says they carry no padding-dependent
  invariant, but nothing checks it. That is F-11 (issue #228): add
  compile-time size checks.
- geiger reports one unsafe impl (`1/4`), grep finds three. Both are recorded;
  the three are the complete list.

Other `unsafe` in the build is in dependencies. Those are not justified line by
line here; they are covered by the dependency review below. geiger's dependency
rows show that most functions in our dependency closure are safe, and the rows
for the unsafe-using dependencies are in the raw output.

Strings that mention `unsafe` (CSP `'unsafe-inline'`, `unsafe path in manifest`
error text, test names) are not `unsafe` code.

## Semgrep rules

File: `.semgrep/wyrd.yml`. Run in the `Security · secrets, SAST` job by
`pipx run semgrep scan --config .semgrep/wyrd.yml --error`, next to the existing
`p/default` run.

The workspace already denies `clippy::unwrap_used`, `clippy::expect_used` and
`clippy::panic` (`[workspace.lints]`, in every crate that inherits it), and CI
runs clippy with `-D warnings`. That is the primary control. The Semgrep rules
cover what clippy cannot: an explicit `allow` in production code, and the
exact call sites on input paths, so a reviewer sees the list.

- `wyrd-no-unwrap-on-input-paths`: `unwrap()` or `expect(...)` in non-test code
  of `wyrd-bundle`, `wyrd-cas`, `wyrd-hub`, `wyrd-sync`, `wyrdd`,
  `wyrd-sensord`, `wyrd-gateway` and `wyrd-adapters`. Excludes `mod tests`
  blocks and `tests/`, `examples/`, `benches/` paths.
- `wyrd-no-lint-allow-for-unwrap-on-input-paths`: an `allow(... unwrap_used
  | expect_used ...)` attribute in the same paths.
- `wyrd-no-shell-out-in-wyrdd`: `Command::new` in `crates/wyrdd` non-test code.

On dev before the review the call rule gave 32 findings (25 inside
`mod tests`, excluded by the `pattern-not-inside`). The remaining hits, and the
new allow rule, were reviewed:

- `redact.rs` compiles constant regular expressions with `expect` (and allows
  the lint on those functions). A failure is a programming error that every
  test run catches. Suppressed with `nosemgrep` comments that name the reason
  (F-7).
- `archive.rs` and `daemon.rs` allow the lint at the top of a `#[cfg(test)]`
  module. Suppressed, reason given (test-only).
- `wyrdd` `spawn_relaunch` runs the `wyrd` binary with fixed arguments and no
  shell, to relaunch an agent after a rewind or fork (ADR-005). The arguments
  are checked by `check_target` (F-3). `doctor_report` runs `wyrd
  __probe-landlock` in a child, because restricting the daemon's own process is
  irreversible. Both are suppressed with a reason (F-6).

Result after these changes: 0 findings. The OSS Semgrep output without login
does not print the matched lines (it shows "requires login"); the findings are
still reported with file and line.

## Dependency review

- `cargo deny check bans licenses sources`: ok.
- `cargo deny check advisories`: four RUSTSEC entries (2026-0391, 0398, 0403,
  0410) against `zstd-sys` 2.1.1 and `zstd-safe` 8.1.0 via `zstd` 0.14.2 in
  `wyrd-cas`. This is F-12; #215 bumps the crates and is queued first. This PR
  does not bump zstd.
- Duplicate versions (`cargo tree -d`): several crates appear in two or three
  versions (for example `hashbrown`, `getrandom`, `rand`, `syn`, `serde`).
  All of them come from transitive dependencies; none is a security concern by
  itself, and the bans policy allows them.
- Workflow and action pinning: every external `uses:` is a 40-character SHA
  with the version in a comment. Local composite actions are referenced by path.
- Build inputs: the Dockerfile (#210) pins Rust and distroless by digest;
  `cargo build --locked` everywhere.
- Not reviewed line by line: the wasmtime, arrow and duckdb trees (the last two
  are behind the `analytics` feature of #220). They are covered by cargo-deny
  and by the release SBOM once #210 lands.

## `wyrd-sensord` capability audit

Source: `deploy/systemd/wyrd-sensord.service` on dev and
`crates/wyrd-sensord/src/main.rs`.

Who holds the capabilities. The unit has no `User=`, so the daemon runs as root.
`CapabilityBoundingSet=` is the ceiling:

| Capability | Why it is granted (documented, or inferred where marked) | Where it is used | Dropped? |
|---|---|---|---|
| `CAP_BPF` | load BPF programs and maps | loader (`crates/wyrd-sensor/src/loader.rs`) | no (F-1) |
| `CAP_PERFMON` | attach tracepoint and perf-based BPF programs | loader; the loader doc says "needs CAP_BPF + CAP_PERFMON (or root)" | no (F-1) |
| `CAP_SYS_RESOURCE` | inferred: raise the locked-memory limit for BPF maps and the ring buffer on kernels that count them against `RLIMIT_MEMLOCK`. Not stated in the unit | loader | no (F-1) |
| `CAP_DAC_READ_SEARCH` | the unit says it is needed for reading tracefs and cgroupfs. Inferred: those entries belong to other users | tracefs and cgroupfs reads | no (F-1) |
| `CAP_SYS_ADMIN` | not explained in the unit or the code. Likely a fallback for kernels without the narrower caps; whether it is needed on the supported kernel matrix is not verified | the unit only | no (F-1) |

The unit says only that the set is "what loading tracepoint programs and reading
tracefs/cgroupfs needs"; the loader says "needs CAP_BPF + CAP_PERFMON (or root)".
The other reasons are inferred. None has been checked capability by capability
on the kernel matrix; that check is part of F-1.

How they are held and limited today:

- `AmbientCapabilities=` is empty, `NoNewPrivileges=yes`.
- The daemon never calls `capset`, `prctl(PR_CAPBSET_DROP)` or `setuid`; grep
  of `crates/wyrd-sensord` and `crates/wyrd-sensor` finds none. Once started it
  keeps every capability in its bounding set for its whole life (F-1).
- Network: `RestrictAddressFamilies=AF_UNIX`, `IPAddressDeny=any`,
  `PrivateNetwork=yes`. The daemon serves only a unix socket
  (`/run/wyrdline/sensord.sock`).
- Filesystem: `ProtectSystem=strict`, `PrivateTmp=yes`, `ProtectKernelModules=yes`,
  `LockPersonality=yes`, `MemoryDenyWriteExecute=yes`,
  `SystemCallArchitectures=native`. `ProtectHome=read-only` keeps home
  directories readable, although the unit comment says there is no access (F-2).
- Not set: `User=`, `SystemCallFilter=`, `ProtectControlGroups` (set to `no`,
  needed for cgroup reads), a socket permission model beyond the directory mode
  (F-2).
- PR #217 (`bugfix/sensord-drop-count`) changes drop counting in the socket
  writer. It does not change privileges.

Conclusion: the sensor's privilege is bounded by systemd, not by the daemon.
A bug in the daemon's socket or decoder code runs with root and the full
bounding set. F-1 and F-2 are the fixes; the attack surface is the local
unix socket, which is same-user per `SO_PEERCRED` (scope B2).

## MCP, Hub and action: findings by PR

- **#209 (MCP server), #218 (confirm), #219 (Studio confirm).** `request_rewind`
  only stores a request and does not rewind (`views.rs`). The agent-facing
  intent socket answers `SessionQuery` variants only (`list_checkpoints`,
  `changes_since`, `explain_effect`, `list_divergences`, `request_rewind`);
  none decides a request. The decision is `POST
  /v1/sessions/{sid}/rewind-requests/{id}` on the daemon API, which sits
  behind the daemon's `require_token` route layer (`crates/wyrdd/src/lib.rs`),
  and the CLI's `wyrd rewind --confirm`. The MCP tools exposed by #209 are
  `list_checkpoints`, `show_changes_since`, `explain_effect`,
  `list_divergences` and `request_rewind`; none confirms. Findings F-4 (dev
  adapter), F-5.
- **dev (`routes/auth.rs`, OIDC login).** Verified: the PKCE verifier, nonce
  and state are stored server-side; the state row is deleted on read (`DELETE
  FROM oidc_logins WHERE state_hash = $1 ... RETURNING`); a login is bound to a
  hashed browser cookie; expired logins are refused; a fresh session id is
  issued per login. Verified on dev.
- **#216 (Hub CI tokens).** Verified on the #216 snapshot: issuer and audience checked on
  every CI token (`jwt::check_ci_token`), trust rule must match the owner
  (and repository when set), token org must match. F-10 is the owner-wide rule.
- **#198 (Hub bundles).** Upload body limit (`DefaultBodyLimit` in `routes/bundles.rs`; the file is
  not on dev) and the bundle reader as above. The Semgrep rules have not been
  run on #198 or #216; they run in CI once those merge.
- **#210 (image), #213 (Helm).** Verified defaults as listed above. The chart's
  NetworkPolicy allows DNS (port 53); an earlier note that it did not was
  wrong.
- **#214 (Studio offline).** Verified the service worker caching scope (app
  shell only) and that the test enforces it.
- **#220 (analytics).** DuckDB is behind a feature and not in the default
  build; its queries use bound parameters (see that PR's description).
- **Action (`integrations/github-action`, dev).** Inputs reach the scripts
  through `env:` (no interpolation into `run:`). Install verifies checksums and
  cosign for signed releases; `auto` accepts unsigned releases with a warning
  (F-8). The sensor step uses `sudo -n`, which needs passwordless sudo on the
  runner (F-9). The default `github-token` is `github.token`; the job's
  permissions apply, and the examples should set `permissions: contents: read`.

## Accepted, with reasons

- F-6: `wyrdd` spawns the `wyrd` binary with argv only. It is the relaunch
  design of ADR-005 and the Landlock probe. Revisit if `wyrdd` gains any other
  process spawn.
- F-7: constant regular expressions compiled with `expect` in `redact.rs`.
- F-9: the action's sensor install needs passwordless sudo on the runner. Without
  it the step fails unless `allow-degraded` is set, in which case the run goes on
  without kernel capture and says so.

## Limits of this review

- Read from branch heads at review time; PRs still open can change.
- No live kernel run: the capability table is from the unit and comments, not
  from a capability-by-capability test on the kernel matrix.
- Semgrep checks call sites and allow attributes on the listed paths. Unwraps
  elsewhere are covered by the clippy deny, not by Semgrep.
- geiger does not run on `wyrdd` (tool error, see above).
