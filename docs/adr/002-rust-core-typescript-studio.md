# ADR-002: Rust core, TypeScript Studio

- Status: Accepted
- Date: 2026-10-14

## Context

The product has a privileged kernel sensor, an unprivileged per-user daemon,
a CLI, a browser UI, and an optional team server. Process boundaries are
also security boundaries.

## Decision

- One Cargo workspace with small crates grouped by responsibility
  (`wyrd-ledger`, `wyrd-cas`, `wyrd-cell`, `wyrd-correlator`, …).
- Three Rust binaries ship to users: `wyrd-sensord` (privileged, minimal),
  `wyrdd` (per user), `wyrd` (CLI). `wyrd-hub` ships as a container.
- Process boundaries follow privilege, not domain: the only privileged
  process is `wyrd-sensord`, kept under ~3 kLOC with no network and no
  persistent storage.
- The Studio is a static SPA embedded into `wyrdd`. In bundle mode it runs
  without any server; the correlator is the same Rust crate compiled to
  `wasm32-unknown-unknown`.
- API and event types are defined once in Rust (`wyrd-schema`) and generated
  for TypeScript; generated files are committed and checked in CI.

## Consequences

- No hand-written TypeScript copies of Rust types.
- The correlator must stay pure (no I/O, no threads, no clocks) to compile to
  Wasm — this is enforced by building it for Wasm in CI.
- A crate is split out only for a privilege, portability, or reuse reason.

## Alternatives considered

- **Next.js** (as in OpsPilot) — server rendering adds nothing to a local,
  offline-capable tool and complicates bundle mode.
- **Tauri desktop app** — out of scope for v1; the browser is enough.
