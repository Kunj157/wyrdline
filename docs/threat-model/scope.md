# Threat model — scope

STRIDE per trust boundary ([overview](../architecture/overview.md#trust-boundaries)).
This document is updated in the same PR as any change to a boundary.

## Assets

| Asset | Why it matters |
|---|---|
| Ledger integrity | The product's evidence; worthless if silently editable |
| Workspace contents and checkpoints | Source code, possibly proprietary |
| Secrets on the host (`~/.ssh`, `.env`, cloud credentials) | Primary exfiltration target of a hijacked agent |
| Host signing key | Signs session heads and bundles |
| Other users' activity on a shared host | Must not leak through the sensor |
| Hub data | Bundles and annotations of a whole organisation |

## Actors

- **Hijacked or careless agent** — the main adversary: follows injected
  instructions, runs arbitrary commands inside the Cell.
- **Malicious content author** — plants prompt injection in repositories,
  web pages, MCP tool results or descriptions.
- **Malicious bundle / policy author** — crafts files that a user opens.
- **Other local user** — tries to read someone else's sessions or events.
- **Network attacker** — between Studio and Hub, or Hub and storage.
- **Insider with Hub access** — over-reaches their role.

## Threats and planned mitigations

| Boundary | STRIDE | Threat | Mitigation | Phase |
|---|---|---|---|---|
| B1 | Tampering | Agent edits ledger or CAS files | Data dir outside Landlock-allowed paths; hash chain; signed head | 2, 3 |
| B1 | Spoofing | Agent forges hook payloads to hide calls | Effects come from the kernel independent of hooks; unattributed effects raise D004 | 5, 10 |
| B1 | Info disclosure | Agent reads secrets and exfiltrates | Landlock read-deny on secret paths, egress allow-list in cgroup BPF, D003 | 9, 10 |
| B1 | Elevation | Agent reaches `wyrdd` API to rewind/apply or change policy | Socket outside the Cell; token file unreadable; MCP server exposes read-only tools + human-confirmed requests | 7, 11 |
| B1 | DoS | Event flood to blind the sensor | Ring-buffer sizing, drop counters in-chain, session marked Degraded | 5 |
| B1 | Tampering | MCP tool description "rug pull" | Description pinning, D009 | 4 |
| B2 | Info disclosure | User A registers user B's cgroup | `SO_PEERCRED` UID must own the cgroup directory | 5 |
| B2 | Elevation | Bug in privileged sensor | Minimal capabilities, small codebase, fuzzed protocol, no network | 5, 14 |
| B3 | Elevation | Escape via symlinks, `/proc/self/fd`, renamed binaries, IPv6, DNS changes | Escape suite on every kernel in the matrix | 9 |
| B3 | Tampering | Modify overlay under a live mount | Freeze + terminate + unmount before any layer change | 3 |
| B4 | Info disclosure | Secrets in shared bundles | Redaction at ingest and export; redaction corpus in CI | 4, 11 |
| B4 | Tampering | Forged or altered bundle | ed25519 signature, per-file BLAKE3, verification on import and Hub ingest | 11, 12 |
| B4 | Elevation | Path traversal / zip-slip / size bombs in bundles | Strict path validation, size limits, fuzzing | 11 |
| B4 | Elevation | Cross-org access in Hub | Org filter on every query, authz matrix tests, audit log | 12 |
| B5 | Info disclosure | Secrets sent to the LLM provider | Only redacted transcript excerpts; prompts scanned in tests | 10 |
| B5 | Tampering | Prompt injection makes the extractor invent claims | Verbatim-quote verification; code decides verdicts (ADR-006) | 10 |
| B6 | DoS | Policy loops or allocates without bound | Fuel and memory limits; failure → deny in Cell mode | 9 |
| B6 | Elevation | Policy reads host files | No WASI filesystem, network, or clock imports | 9 |

## Accepted risks (v1)

- Someone with the host private key can rewrite and re-sign local history.
- IP-based egress rules cannot distinguish names that share CDN addresses.
- Effects outside the Cell that policy allows (e.g. permitted network calls)
  cannot be undone.
- Without BPF-LSM, exec denial is detect-and-stop rather than prevention for
  binaries Landlock does not cover.

## Out of scope

Kernel exploits, a malicious root user, hardware attacks, and compromise of
the LLM provider.
