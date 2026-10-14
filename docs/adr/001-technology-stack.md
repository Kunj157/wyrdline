# ADR-001: Technology stack

- Status: Accepted
- Date: 2026-10-14

## Context

Wyrdline sits between a coding agent and the Linux kernel. It must capture
kernel events at high rates with negligible overhead, manipulate namespaces,
mounts, cgroups and Landlock rulesets, host untrusted WebAssembly policies,
render very large event graphs in a browser, and replay sessions offline.
One developer builds and maintains all of it, so the number of languages and
runtimes has to stay small.

## Decision

| Area | Choice |
|---|---|
| Core, daemon, CLI, sensor, Hub | Rust (stable); eBPF crate on pinned nightly |
| eBPF | Aya + aya-ebpf + aya-log, CO-RE via BTF, `bpf-linker` |
| Async / HTTP | Tokio, Axum, tower |
| Local storage | SQLite (bundled, WAL) per session; content-addressed chunk store |
| Hashing / signing | BLAKE3, ed25519 |
| Chunking / compression | FastCDC, zstd |
| Serialisation | canonical CBOR on disk, JSON on the API |
| Sandbox | cgroup v2, user + mount namespaces, overlayfs, Landlock |
| Agent protocols | MCP via `rmcp`, agent hook payloads, OTLP GenAI spans |
| Policies | WebAssembly component model on wasmtime (WASI 0.2 guests) |
| LLM | Claude API with structured outputs; provider `none` must work |
| Studio | React 19, TypeScript strict, Vite, TanStack, Tailwind v4, Motion, three.js/R3F, WebGPU |
| Collaboration | Automerge |
| Hub | Axum, PostgreSQL, S3-compatible storage, DuckDB |
| Delivery | GitHub Actions, vmtest kernel matrix, cargo-dist, cosign, SBOM |

Exact versions are pinned in `Cargo.lock`, `pnpm-lock.yaml`, and
`rust-toolchain.toml` at bootstrap and only upgraded on dedicated branches.

## Consequences

- Two languages (Rust, TypeScript) cover everything; the correlator is shared
  between the daemon and the browser through Wasm.
- Nightly Rust is confined to one crate; everything else builds on stable.
- Linux only for capture (macOS/Windows are explicitly out of scope).

## Alternatives considered

- **Go + cilium/ebpf** — mature eBPF loader, but no single language for
  kernel programs, userspace, and browser Wasm; weaker Wasm story.
- **Python** — fast to write, but unsuitable for the sensor hot path and the
  browser correlator.
- **libbpf (C) programs** — well-trodden, but adds C and clang to the
  toolchain; Aya keeps the eBPF code in Rust and shares structs.
