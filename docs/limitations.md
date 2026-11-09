# Limitations

What Wyrdline does not do, what it does only partly, and what degrades
when the machine lacks a capability. Everything here is also stated
where it applies (the threat model, the policy guide, `wyrd doctor`);
this page collects it.

## Out of scope for 1.0

By decision (plan scope, not for lack of time):

- **Linux only.** No macOS (Endpoint Security) or Windows (ETW) sensor.
  The eBPF sensor, Landlock, cgroup v2, and overlayfs are Linux.
- **No TLS interception.** Wyrdline does not read the agent's traffic
  to its model provider. Prompts, tool calls, and messages come from
  the agent's own hooks and the MCP gateway; network effects are
  recorded as connections (address, port, resolved name), not content.
- **No conversation rewind.** `wyrd rewind` restores the *file system*
  to a checkpoint and can relaunch the agent on the new branch
  (`--relaunch`); the agent's conversation is not restored. Use the
  agent's own resume or fork flags in the relaunched command if it has
  them.
- **No kernel modules.** Everything in the kernel is eBPF loaded by
  `wyrd-sensord`, plus Landlock and cgroup v2, which the stock kernel
  provides.
- **Network side effects are not undone.** A request that left the
  machine has happened. Wyrdline denies forbidden connections before
  they happen (cgroup BPF, Landlock ports) and flags allowed ones; it
  cannot reverse them. The same holds for anything the agent did
  outside the overlay through an allowed channel. A rewind or fork does
  not undo them either: `wyrd rewind` and `wyrd fork` warn about each
  network effect after the checkpoint, denied attempts included. The
  `state.checkpoint.rewound` event lists them in `irreversible_seqs`; a
  fork's event does not. Studio's rewind and fork dialogs list the
  effects before you confirm, by the same branch rule as the CLI (in a
  bundle or replay, which has no branch tree, every effect after the
  checkpoint). They count allowed connections and writes outside the
  Cell; the CLI also lists denied connection attempts.
  The daemon API's rewind and fork responses return the same list as the
  CLI in `irreversible_seqs`, except a relaunch (202), which runs detached
  ([quick start](quick-start.md#4-rewind-fork-apply-20-s)).
- **No node-wide or cluster capture.** No Kubernetes operator, no
  DaemonSet mode: Wyrdline records sessions it starts with `wyrd run`
  (locally or in the GitHub Action).
- **No billing, no multi-tenant SaaS.** The Hub has organisations and
  roles; that is the extent of tenancy.
- **Not an EDR or SIEM.** The sensor only looks at cgroups registered
  for a session; it does not watch the rest of the machine.
- **No model is fine-tuned.** Claim extraction uses a hosted model with
  structured output, and only to *extract* claims; every verdict is
  decided by code ([ADR-006](adr/006-llm-extracts-code-decides.md)).
- **No desktop app.** Studio is a web UI served by `wyrdd` on loopback.

## Capture is only as complete as the machine allows

`wyrd doctor` reports each capability; `wyrd run` records what was
available in the session's `meta.session.started` event and marks the
session **Degraded** when something is missing or lost mid-run. A
Degraded session is never shown as complete: the CLI warns, the Studio
labels it, and the GitHub Action's sensor step refuses to run the agent
without kernel capture unless `WYRD_ALLOW_DEGRADED=true`.

| Missing | Effect on a session |
|---|---|
| `wyrd-sensord` not running, or no systemd user manager (the Cell gets no cgroup of its own) | No kernel effects at all. Tool calls, MCP traffic, OTLP spans, policy decisions, and checkpoints are still recorded; the correlator has nothing to attribute; the divergence rules that judge effects (writes, reads, connects, unlinks: most of D002–D008) cannot fire; `on-effect` and `exec-deny` do not run; the egress allow-list is **not** enforced. |
| Sensor connection lost mid-run | `meta.sensor.lost`, Degraded; effects between loss and `meta.sensor.resumed` are missing. |
| Ring-buffer overflow | Effects were dropped; the count is recorded in-chain (`drops_since_last` on the next event) and `wyrd run` warns. The ring buffer is 64 MiB by default (`wyrd-sensord --ringbuf-mb`); in the 1.0 measurements (run 2) it received 200 000 events paced at 49 863 events/s with none dropped, and 216 439 events/s unpaced with none dropped ([performance](results/perf-v1.0.md)). Events a slow client's queue refuses are counted the same way (#217). A loss after a session's last captured effect has no later effect to carry the count, so it does not reach the ledger; fixing that needs a ledger schema change. |
| Landlock ABI < 4 | TCP port rules cannot be enforced; the session says so and is Degraded. |
| Landlock, cgroup v2, or overlayfs in user namespaces absent | No Cell mode: `wyrd run` needs `--observe`, which records intent (and effects, if the sensor runs) but has no sandbox, no checkpoints, no rewind, and does not enforce the policy's kernel rules. |
| BPF LSM not enabled | Exec denial is detect-and-stop (below). Nothing else changes; BPF LSM is optional. |
| `cgroup_sock_addr` hooks absent (kernel < 4.17) | No kernel egress filtering. |

## Enforcement boundaries

- **Egress rules are addresses, not names.** `egress-allow-hosts` is
  resolved by `wyrd run` (again every 60 s) into addresses for the
  cgroup `connect4`/`connect6` programs. A CDN or shared address that
  an allowed name resolves to is allowed for every other name on it.
  DNS itself is not intercepted. A connect is recorded with a host name
  (`dns_name`) only when its address is one an allowed name resolved to in
  that session. The name is `wyrd run`'s own resolution of the policy's
  allowed hosts, not an answer the agent received: an address the agent
  reached through another name, or one no allowed name resolved to, is
  recorded with the address alone.
- **UDP.** The connect programs decide `connect()`; TCP and connected
  UDP are covered, unconnected UDP (`sendto` without `connect`) is not.
- **Exec denial is detect-and-stop.** `exec-deny` and `on-effect` run
  on the effect stream after the kernel reported the event: the
  forbidden program may run for a moment before the session is frozen
  and killed. File and network rules are enforced *before* the fact
  (Landlock, cgroup BPF). Renamed copies of a listed program are caught
  by size and BLAKE3 of the binary found on `PATH` at session start,
  not by content in general.
- **Read denial is a complement.** Landlock grants; `fs-read-deny` is
  compiled as "read everything except": names inside a denied directory
  can still be listed, and files created later next to a denied path
  are not readable. Patterns such as `**/.env*` cannot be expressed in
  Landlock and are left to tool-call and effect-time checks (the
  kernel-stage `guard.decision` lists them).
- **Tool-call decisions depend on the agent reporting the call.** The
  hook layer only sees what the agent's hooks send. A tool the agent
  runs without a hook is still subject to the kernel layers, and its
  effects show up as unattributed (D004).
- **`ask` cannot wait at effect time.** From `on-effect`, `ask` is a
  deny: the effect has already happened.
- **The policy sandbox is a budget, not a proof.** A policy that traps,
  exceeds its fuel or 32 MiB, or returns a malformed decision is a deny
  in Cell mode; `kernel-rules` failing stops the session from starting.

## Evidence, not proof

- **Verification is tamper *evidence*.** `wyrd verify` proves that a
  ledger or bundle matches the hash chain and the signature of the key
  shown. Whoever holds the host private key
  (`keys/host.key`, mode 0600) can rewrite and re-sign local history.
  Trusting a key is the receiver's decision; the fingerprint is printed
  for comparison.
- **Attribution has confidence levels.** `direct` (1.0) comes from a
  matched exec or a touched target path; `window` (0.8) from a single
  open call window; `ambiguous` lists candidates and picks none. The
  kernel keeps the first 8 arguments of each exec, up to 63 bytes of
  each, so a wrapped
  command (`bash -c -l "source …; eval '<cmd>'"`) is matched by its
  window rather than its text. The published precision and recall
  ([attribution](results/attribution.md)) are on synthetic sessions
  and are an upper bound.
- **Divergence rules are heuristics with evidence.** Each finding cites
  ledger `seq` numbers and has a confidence; `wyrd triage` marks false
  positives, which stay in the ledger. D001 (claim without evidence)
  depends on recognising the test runner — extend `test_runners` in
  `.wyrdline/config.toml` for unusual ones. D002 (out-of-scope write)
  without a `scope.toml` falls back to the scope read from the prompt,
  at lower confidence. The regex claim extractor alone scores F1 0.86
  on the committed eval set ([baseline](results/evals-baseline.json));
  the model-backed extractor adds recall, costs money (capped at
  `WYRD_LLM_MAX_COST_USD`, 0.50 by default; the cost per session has not
  been measured from live runs, [#212](https://github.com/Kunj157/wyrdline/issues/212)), and only ever adds claims
  whose quote is a verbatim substring of the transcript.
- **D003 pairs a secret read with any connect in the agent's tree.** A
  secret read (kernel-reported, a tool call whose path argument names a
  secret, or a command that names one) is paired with a connect when the
  connect comes within `egress_window_ms` (10 s by default) and both
  processes are in the agent's process tree. The tree includes the agent's
  root process. The demo scenario (a `.env` read by one tool call, then
  `curl` from another) needs only sibling pairing. The root-inclusive tree
  is a deliberate superset of that. It exists because the agent root can
  read a tool's output, which may contain the secret, and send it over its
  own socket. So a read by a tool subprocess also pairs with a connect the
  agent root makes itself, for example to its model or telemetry endpoint.
  That is intended. It also means an unrelated root connect in the window
  can be paired with a read it had nothing to do with. Real-agent example
  (2026-11-08, the first real-agent run of the system test): `npm test`
  read `~/.npmrc`, and the agent root's telemetry connect, denied by
  policy, came within 10 s. D003 fired twice (critical). Mark such a
  finding as a false positive with `wyrd triage` when the connect is known
  to be unrelated; the rule itself is not narrowed. The eval case
  `d003-pos-subprocess-read-root-connect` pins this pairing, so a change
  to it has to be deliberate.
- **Redaction is pattern-based.** The `standard` level catches known
  secret formats, auth headers, URL passwords, and high-entropy tokens;
  `strict` adds user and host names. A secret that looks like ordinary
  text is not caught; the contents of secret-classified paths
  (`.env*`, keys, credential files) are never captured at all, and
  strict exports withhold such files from the bundle. Review a bundle
  before sharing it outside your team.

## Adapters

What each agent reports is in [adapter coverage](adapter-coverage.md).
The short version: Claude Code is wired automatically and verified
against recorded payloads and real runs; Codex CLI and Gemini CLI need
a one-time hook registration and have **not been verified in a live
run** on the development machine; a generic MCP client is covered only
for the servers you wrap with `wyrd mcp-proxy`; `wyrd mcp` lets an agent
read its own session and request a rewind, which a person must confirm;
OTLP covers `execute_tool` spans. Kernel effects are captured for every agent
regardless.

The MCP gateway's tool pins (a tool that changes its description or schema
is flagged, D009) are kept in each session's `mcp-pins.json`, so they survive
a restart of that session's gateway (#233). They are per session: a server
that changes between two separate `wyrd run`s is not flagged. `wyrd mcp`
returns session text as quoted data, cut at 200 items and 32 KiB per block
(#235).

## Studio

- WebGPU is used when the browser offers it, with Canvas2D and SVG
  fallbacks and a table view. The committed measurements
  ([studio performance](results/studio-perf.md)) are: Canvas2D at 60 fps
  (p95 frame 16.8 ms) with 200 000 events in headless Chromium; WebGPU
  on a CPU software rasteriser (SwiftShader) at 30 fps (p95 frame
  33.2 ms) with the same events. The 60 fps target on a real GPU has not
  been measured: there is no GPU on the development machine or the CI
  runners.
- Monotonic timestamps above 2^53 ns (about 104 days of uptime) lose
  precision in the browser; ordering always uses `seq`.
- Offline replay (`?offline=`) accepts an events export from the same
  origin as Studio only. `.wyrd` bundles open in the browser through
  `wyrd open` or Studio's file picker and drag-and-drop; they are
  verified before anything is shown, and the view is read-only.

## Operational

- **No `wyrd backup` or `wyrd restore` yet** ([#318](https://github.com/Kunj157/wyrdline/issues/318)).
  To back up, stop the sessions (`wyrd stop`, or wait for them to finish),
  then copy the data directory (`$WYRD_DATA_DIR`, else
  `$XDG_DATA_HOME/wyrdline`, else `~/.local/share/wyrdline`): the ledgers
  with their `-wal` and `-shm` files, `cas/`, `sessions/`, and
  `keys/host.key`, which stays mode 0600. Overlay `work/` directories may be
  root-owned scratch space and need not be copied. The daemon token is not
  in the data directory (`wyrd token path`).
- **Checkpoints miss their latency target for large changes.** For 200
  files and a 50 MB delta the checkpoint p95 is 158.7 ms on the development
  machine, against a 150 ms target, and 260–530 ms on a GitHub-hosted
  runner, whose disk is slower. The target is missed and tracked in
  [#239](https://github.com/Kunj157/wyrdline/issues/239)
  ([performance](results/perf-v1.0.md)).
- `wyrd-sensord` runs as root, not as a dedicated user. After it attaches
  its programs it keeps only `CAP_BPF`; the unit's comments explain why it
  is not a dedicated user yet ([install](install.md#3-install-the-sensor)).
- `wyrd run` owns a live session. `wyrdd` reads the ledger read-only
  and reaches a running session only through its intent socket; a
  rewind or fork with relaunch requested from Studio starts a detached
  `wyrd rewind … --relaunch` whose output goes to `relaunch.log` in the
  session directory.
- Applying a subset of paths (`wyrd apply --paths`) is not implemented;
  `apply` writes a whole checkpoint or branch.
- `wyrd gc` deletes the content store's chunks that no session's checkpoint
  refers to, or with `--dry-run` only reports them. It waits at most ten
  seconds for a checkpoint or import in progress, then fails without
  deleting ([#296](https://github.com/Kunj157/wyrdline/issues/296)). Without
  `--keep-days` or `--keep-sessions` it never deletes a session: history is
  kept as long as its session exists. With them, closed sessions beyond the
  limits are removed, with their ledgers, before the sweep
  ([#318](https://github.com/Kunj157/wyrdline/issues/318);
  [retention](cas-format.md#garbage-collection)).
- The sensor sees exactly the syscalls its programs trace
  ([sensor](architecture/sensor.md)); file operations issued through
  other interfaces (for example `io_uring`) are not among them. Reads
  are recorded only for secret-looking paths; a write is recorded as
  the successful open for writing, not as the bytes written.
