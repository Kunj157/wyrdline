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

Pre-alpha. Work proceeds phase by phase; see the [roadmap](#roadmap).
Phases 0–9 are done: `wyrd doctor`, the hash-chained ledger and
content-addressed store, the Cell (checkpoint, rewind, fork, diff, apply),
the intent plane — agent hooks, an MCP gateway (stdio and Streamable
HTTP servers), OTLP traces, redaction, and a checkpoint after every tool
call — the effect plane: an eBPF sensor (`wyrd-sensord`) that records
every exec, file write, rename, delete, and connect of the agent's
process tree, with drops counted in the ledger — the correlator, which
attributes each kernel effect to the tool call that caused it, natively
and in WebAssembly — and `wyrdd`, the per-user daemon with an
authenticated HTTP API, a resumable live event stream, and an
[OpenAPI contract](docs/api/openapi.json) — and **Studio**, the web UI
the daemon serves: the Causal River (WebGPU, with Canvas2D and SVG
fallbacks), an inspector for every event, live mode, rewind, fork, diff,
and apply, and offline replay of exported sessions with the correlator
running in the browser ([performance](docs/results/studio-perf.md)) — and the
**Guard**: policies as sandboxed WebAssembly components that decide every
tool call (allow, deny, or ask a human), with their rules enforced in the
kernel — Landlock for files and ports, cgroup BPF for network egress, an
exec observer that stops forbidden programs — and backtests of recorded
sessions ([policy guide](docs/policies/README.md),
[escape suite and latency](docs/results/enforcement.md)):

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
```

Capture costs under 2 % on a `cargo build` ([measurements](docs/results/perf-sensor.md)).

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
- **Delivery:** GitHub Actions, kernel-matrix VM tests, cargo-dist, cosign,
  SLSA provenance, CycloneDX SBOM

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

- [Architecture overview](docs/architecture/overview.md)
- [The Cell](docs/architecture/cell.md), [intent capture](docs/architecture/intent.md),
  [agent adapters](docs/architecture/adapters.md), [redaction](docs/architecture/redaction.md),
  [the eBPF sensor](docs/architecture/sensor.md), [the correlator](docs/architecture/correlator.md),
  [the daemon and its API](docs/architecture/daemon.md)
- [Ledger format](docs/ledger-format.md), [CAS format](docs/cas-format.md),
  [bundle format](docs/bundle-format.md)
- [Writing a policy](docs/policies/README.md)
- [Architecture decision records](docs/adr/README.md)
- [Threat model](docs/threat-model/scope.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and
[DEVELOPMENT_WORKFLOW.md](DEVELOPMENT_WORKFLOW.md).

## License

[MIT](LICENSE). The eBPF programs are dual-licensed MIT OR GPL-2.0.
