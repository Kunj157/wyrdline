# Wyrdline

**A causal flight recorder and rewind engine for AI coding agents.**

> Every agent leaves a thread. Wyrdline lets you follow it, cut it, and
> re-weave it.

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

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and
[DEVELOPMENT_WORKFLOW.md](DEVELOPMENT_WORKFLOW.md).

## License

[MIT](LICENSE). The eBPF programs are dual-licensed MIT OR GPL-2.0.
