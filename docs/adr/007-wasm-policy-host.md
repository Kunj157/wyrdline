# ADR-007: Wasm policy host — wasmtime, fresh instance per call, no WASI

- Status: Accepted
- Date: 2026-10-19

## Context

Plan §17.3 makes policies WebAssembly components (`policies/wit/policy.wit`)
so users can write them in any language and so a policy cannot harm the
machine it guards. The host runs inside `wyrd run` (pre-tool-call and
effect-time decisions) and `wyrdd` (backtests on uploaded policies), so the
component is untrusted code in a privileged decision path. Requirements:
bounded time and memory, no ambient authority, deterministic decisions
(backtests must give identical output for the same policy and session),
and failure to deny.

## Decision

- **Runtime: wasmtime** (component model, Cranelift), the reference
  implementation of the component model and WIT. It is a heavy dependency
  (compile time, binary size); it is confined to `wyrd-policy`.
- **Fresh instance per call** from a pre-linked `InstancePre`: no state
  carries over between decisions, so one trapped call cannot poison the
  next and the order of calls does not matter. Instantiation costs ~20 µs.
- **Fuel, not wall time**, bounds each call (`FUEL_PER_CALL`, ~8 ms of
  guest code on the dev box): deterministic across machines and loads.
  Memory is capped at 32 MiB per instance; growing past it traps.
- **No WASI.** The only import a policy gets is `log` (bounded). Every
  other import the guest's toolchain links in (WASI stdio, clocks,
  environment, filesystem, sockets) is defined as a trap. A policy
  therefore has no filesystem, network, clock, or entropy, and Rust
  guests still build with the standard library.
- **Every failure denies**: trap, fuel exhausted, memory cap, or a
  malformed component → `deny("policy error: …")`. `kernel-rules` failing
  means the session cannot be enforced, and the caller refuses to start it.
- Policy identity is the file stem; its version is a BLAKE3 prefix of the
  component bytes, recorded with every `guard.decision`.

## Consequences

- Policies are reproducible: a backtest years later gives the same
  decisions as the live run.
- Guest code that happens to call a WASI function (printing, reading the
  time) traps. Authoring docs say so; `log` is the only output.
- Compiling a component takes tens of milliseconds and happens once per
  load, not per call.

## Alternatives considered

- **wasmtime-wasi with an empty context** — still exposes real clocks and
  entropy, and links a large WASI implementation for no benefit.
- **`no_std` guests** — would avoid WASI imports at the source but make
  policies much harder to write.
- **wasmi / an interpreter** — smaller, but slower and without a native
  component-model implementation.
- **Rego/CEL or a TOML rule language** — simpler, but the plan wants
  policies as code in any language, sandboxed by construction.
