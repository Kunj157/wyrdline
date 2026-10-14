# ADR-003: Landlock + cgroup BPF as primary enforcement, BPF-LSM optional

- Status: Accepted
- Date: 2026-10-14

## Context

Policies must be enforced in the kernel, not only by asking the agent's own
harness. BPF-LSM is the most expressive option, but it requires `bpf` in the
kernel's active LSM list, which stock distributions often do not enable. The
development machine (Ubuntu 24.04, kernel 6.17) reports
`lockdown,capability,landlock,yama,apparmor,ima,evm` — Landlock yes, BPF-LSM
no.

## Decision

Enforcement is layered so that it works on stock kernels:

1. **Filesystem:** a Landlock ruleset is applied to the agent process before
   `exec` (read / write / execute per path hierarchy). The ABI version is
   negotiated at runtime; a downgrade is recorded and the session is marked
   Degraded if a requested right cannot be enforced.
2. **Network:** `cgroup/connect4` and `cgroup/connect6` BPF programs attached
   to the session cgroup check destinations against an LPM-trie allow-list.
   Host names in policies are resolved by `wyrdd` and refreshed on TTL.
   Landlock TCP connect rules add port-level defence in depth.
3. **Exec:** Landlock `EXECUTE` rights plus an exec observer that freezes the
   cgroup on a forbidden exec (detect-and-stop, documented as such).
4. **BPF-LSM** programs are loaded only when `wyrd doctor` finds `bpf` in the
   LSM list. They are never required.

Pre-tool-call checks (hooks and the MCP gateway) remain the first line, as
they can also *ask* the human.

## Consequences

- Works unprivileged for the filesystem part and needs only `CAP_NET_ADMIN`
  / `CAP_BPF` in `wyrd-sensord` for the network part.
- Host-name egress rules are IP-based in the kernel: CDN-hosted names can
  share IPs with other names. Documented as a limitation.
- Effect-time denials can stop a session but cannot undo an effect.

## Alternatives considered

- **BPF-LSM only** — unavailable on the default kernels of the main target
  distribution.
- **seccomp user notification** — syscall-level, racy for path checks
  (TOCTOU), and expensive.
- **Containers (Docker/Podman)** — heavier, changes the agent's environment,
  and still needs policy enforcement inside.
