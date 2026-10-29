# Wyrdline

**A causal flight recorder and rewind engine for AI coding agents.**

> Every agent leaves a thread. Wyrdline lets you follow it, cut it, and
> re-weave it.

**[Website and live demo](https://kunj157.github.io/wyrdline/)**: the
real Studio replaying a recorded session in your browser.

Coding agents (Claude Code, Codex CLI, Gemini CLI, any MCP client) run real
commands on real machines. What they *say* they did and what they *actually*
did can differ, and today there is no independent record and no undo for
effects that happen outside the agent's own edit tool.

Wyrdline runs an agent inside a **Cell** (a cgroup-scoped copy-on-write
sandbox) and records three planes into one tamper-evident **Ledger**:

| Plane | Source | Example |
|---|---|---|
| Intent | agent hooks, MCP gateway, OpenTelemetry GenAI spans | `Bash("pnpm test")` |
| Effect | eBPF in the kernel, scoped to the agent's cgroup | `exec pnpm`, `write src/cart.ts`, `connect 1.2.3.4:443` |
| State | content-addressed checkpoint after every tool call | checkpoint 9 = manifest `b3:…` |

From that it can:

- **Correlate** every effect to the tool call that caused it, with confidence.
- **Rewind** the workspace to any checkpoint, **fork** a new branch, **diff**
  branches, and **apply** the one you want.
- **Guard** sessions with policies written as WebAssembly components and
  enforced in the kernel (Landlock + cgroup BPF), and **backtest** a policy
  against a recorded session.
- Flag **intent–effect divergence**: "all tests pass" with no test run,
  writes outside the task's scope, a secret read followed by a network
  connection, effects that never appear in the transcript.
- **Export** a redacted, signed `.wyrd` bundle that replays **offline in the
  browser** (the correlator runs as Wasm), and annotate it with teammates
  through local-first CRDT sync.
- Capture agent runs **in CI** through a GitHub Action and roll them up in a
  team **Hub**.

## Status

Pre-1.0: the 1.0 user documentation is in [`docs/`](#documentation). The
roadmap is [below](#roadmap); what 1.0 does not do is in
[limitations](docs/limitations.md). The capabilities, in short:

- **Capture:** `wyrd run` sandboxes an agent in a Cell, records its tool
  calls (hooks for Claude Code, an MCP gateway, OTLP), and records every
  exec, write, rename, delete, and connect of its process tree through
  an eBPF sensor, with drops counted in the ledger
  ([adapter coverage](docs/adapter-coverage.md)).
- **Analysis:** the correlator attributes each kernel effect to the tool
  call that caused it, natively and in WebAssembly; divergence rules
  compare what the agent said with what happened.
- **Control:** checkpoints after every tool call, with rewind, fork,
  diff, and apply; policies written as WebAssembly components decide each
  tool call (allow, deny, or ask a human) and their kernel rules are
  enforced by Landlock and cgroup BPF ([policy guide](docs/policies/README.md),
  [enforcement results](docs/results/enforcement.md)).
- **Agents can ask, not act:** `wyrd mcp` lets an agent inspect its own
  session and request a rewind; a person confirms or rejects it.
- **Studio:** the web UI served by `wyrdd`: the Causal River, an
  inspector for every event, live sessions, rewind and apply, and offline
  replay of exported sessions and verified `.wyrd` bundles in the browser
  ([performance](docs/results/studio-perf.md)).
- **Sharing:** signed, redacted `.wyrd` bundles that anyone can verify
  with `wyrd verify`, and a team Hub for uploading and searching them.

```
wyrd run -- claude -p "fix the failing cart test"   # hooks injected, workspace sandboxed
wyrd events <session> --plane effect                # what actually happened in the kernel
wyrd graph <session>                                # which tool call caused which effect
wyrd explain <session> <seq>                        # one effect: process chain and cause
wyrd tail <session>                                 # follow a live session
wyrdd                                               # API on a same-UID socket and 127.0.0.1:7717
wyrd studio                                         # open Studio in the browser
wyrd checkpoints <session>                          # one checkpoint per tool call
wyrd rewind <session> c3 && wyrd diff <session> c3 head
wyrd apply <session> head                            # bring the result into the real tree
wyrd run --policy policies/default.wasm -- claude …   # decide, ask, and enforce in the kernel
wyrd ask <session>                                   # answer what the policy asks
wyrd policy backtest policies/strict.wasm <session>  # what a stricter policy would have stopped
wyrd export <session> -o session.wyrd               # signed bundle, strict redaction by default
wyrd verify session.wyrd && wyrd import session.wyrd # check every hash, then add read-only
wyrd mcp                                             # MCP server for the agent: inspect itself, request a rewind
wyrd rewind <session> --requests                     # the agent's rewind requests; --confirm / --reject <id>
wyrd push session.wyrd --hub https://hub.example --org acme  # share it on a team Hub (token in WYRD_HUB_TOKEN)
```

Capture's overhead on a `cargo build` was between −1.3 % and +0.1 % across
two runs on a shared machine ([performance](#performance)).

## Performance

Measured for 1.0 on ws11, a shared 24-core machine: other users' sessions
ran during every measurement, with load averages of 4–7 during the Cell
runs and 2–10 during the sensor runs. The numbers are noisy. The sensor
overhead tests compare interleaved runs and report medians. Run 2 is the
committed revision; run 1 is the earlier code, which differs only in the
victim-shop wrapper and the paced test. Method, both runs, and how to
reproduce them: [performance at v1.0](docs/results/perf-v1.0.md).

| Target | Measured | Result |
|---|---|---|
| Event loss at 50 k events/s | 49 863 events/s (run 2): 200 000 received, 0 dropped | met |
| Checkpoint p95, 200 files, 50 MB delta (target ≤ 150 ms) | p95 7.22 s; median 5.52 s over 20 rounds | **missed**, tracked in [#239](https://github.com/Kunj157/wyrdline/issues/239) |
| Rewind p95, 1 GB workspace (target ≤ 2 s) | p95 1.16 s; median 0.93 s over 10 rounds | met |
| Denied-connect latency, p99 | 2.3–2.6 µs | met; the report explains why it is not compared with allowed connects |
| Allowed-connect latency added, p99 (target ≤ 50 µs) | +0.2 µs (run 2), +1.4 µs (run 1) | met |
| Overhead on `cargo build`, captured (target ≤ 5 %) | +0.1 % (run 2), −1.3 % (run 1) | met |
| Overhead on the `pnpm test` workload, captured | −0.5 % (run 2), −0.2 % (run 1), on a 120 ms workload | met, but the workload is too short to trust ([#240](https://github.com/Kunj157/wyrdline/issues/240)) |
| Judge cost per session | not measured: needs a live `ANTHROPIC_API_KEY` | not measured ([#212](https://github.com/Kunj157/wyrdline/issues/212)) |

**The checkpoint target is missed.** Checkpoint p95 is 7.22 s against a
150 ms target, about 40× over. Even the fastest of the 20 rounds (5.00 s)
misses it. The cost has not been profiled yet; it is tracked in
[#239](https://github.com/Kunj157/wyrdline/issues/239).

On the syscall-dense stress workload (about 7 000 effects per run), a captured
session costs +9.4 % (run 2) and +9.7 % (run 1). That is above the 5 % budget
the plan sets for build and test workloads, and inside the 12 % regression
bound. The earlier sensor numbers and the design history are in
[sensor performance](docs/results/perf-sensor.md) and
[Studio performance](docs/results/studio-perf.md).

## How it compares

| | Kernel-level evidence | Tool-call attribution | Rewind / fork incl. subprocess effects | Kernel policy enforcement | Claim vs effect checks | Offline replay |
|---|---|---|---|---|---|---|
| eBPF agent observability (e.g. AgentSight) | ✓ | ✓ | – | – | – | – |
| Generic runtime security (Tetragon, Falco) | ✓ | – | – | ✓ | – | – |
| Agent sandboxes | – | – | partial | ✓ | – | – |
| Built-in agent checkpoints | – | – | own edits only | – | – | – |
| LLM tracing (LangSmith, Langfuse) | – | self-reported | – | – | – | – |
| **Wyrdline** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

## Technology

- **Core:** Rust — Aya (eBPF, CO-RE), Tokio, Axum, SQLite, BLAKE3, FastCDC,
  Landlock, overlayfs, cgroup v2
- **Agents:** MCP (`rmcp`), agent hooks, OpenTelemetry GenAI conventions,
  Claude API with structured outputs for claim extraction
- **Policies:** WebAssembly component model (wasmtime, WIT, WASI 0.2)
- **Studio:** React 19, TypeScript, Vite, TanStack, Tailwind v4, WebGPU,
  Automerge, Rust correlator compiled to Wasm
- **Hub:** Axum, PostgreSQL, S3-compatible storage, DuckDB, Helm
- **Delivery:** GitHub Actions, kernel-matrix VM tests, reproducible builds,
  cosign, SLSA provenance, CycloneDX SBOM

## Requirements

Linux with BTF and cgroup v2 (kernel ≥ 5.15). Landlock is used for filesystem
enforcement; BPF-LSM is used only if the kernel enables it. `wyrd doctor`
reports exactly what is available and what degrades without it.

## Roadmap

| Phase | Milestone |
|---|---|
| 0 | Planning and repository bootstrap |
| 1 | Foundation: workspace, CI, event schema, `wyrd doctor` |
| 2 | Ledger and content-addressed store |
| 3 | Cell: sandbox, checkpoint, rewind, fork, apply |
| 4 | Intent plane: agent adapters, MCP gateway, OTLP, fakeagent |
| 5 | Effect plane: eBPF sensor |
| 6 | Correlator |
| 7 | Daemon API, SSE, CLI |
| 8 | Studio and the WebGPU Causal River |
| 9 | Guard: Wasm policies and kernel enforcement |
| 10 | Divergence engine and evals |
| 11 | Bundles, signing, offline replay, MCP server |
| 12 | Collaboration and Hub |
| 13 | CI capture, containers, Kubernetes |
| 14 | Hardening, performance, launch |

## Documentation

- **Using Wyrdline:** [install](docs/install.md), [quick start](docs/quick-start.md)
  (install to a recorded, inspected, exported session), [adapter coverage](docs/adapter-coverage.md)
  (what each agent reports), [the Hub](docs/hub.md) (team uploads, search, and
  analytics), [limitations](docs/limitations.md), [FAQ](docs/faq.md)
- [Architecture overview](docs/architecture/overview.md)
- [The Cell](docs/architecture/cell.md), [intent capture](docs/architecture/intent.md),
  [agent adapters](docs/architecture/adapters.md), [redaction](docs/architecture/redaction.md),
  [the eBPF sensor](docs/architecture/sensor.md), [the correlator](docs/architecture/correlator.md),
  [the daemon and its API](docs/architecture/daemon.md), [the Hub](docs/architecture/hub.md),
  [collaborative annotations](docs/architecture/annotations.md)
- [Ledger format](docs/ledger-format.md), [CAS format](docs/cas-format.md),
  [bundle format](docs/bundle-format.md)
- [Writing a policy](docs/policies/README.md)
- [Architecture decision records](docs/adr/README.md)
- [Threat model](docs/threat-model/scope.md)
- [Releases: assets, verification, reproducibility](docs/release.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and
[DEVELOPMENT_WORKFLOW.md](DEVELOPMENT_WORKFLOW.md).

## License

[MIT](LICENSE). The eBPF programs are dual-licensed MIT OR GPL-2.0.
