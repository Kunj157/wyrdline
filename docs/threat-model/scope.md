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

## Boundaries added for 1.0

Reviewed in [the 1.0 security review](review-2026.md). Each row names the
PR it depends on; "dev" means the code is on `dev` today. Findings are
numbered `F-…` there.

| Boundary | STRIDE | Threat | Mitigation and status | Depends on |
|---|---|---|---|---|
| B4 bundles | Tampering, Elevation | Forged, traversing, or oversized bundle | Reader verifies the ed25519 signature over `manifest.json`, per-file hashes, path rules (`..`, absolute, symlink, hardlink, device), entry and total size limits, and a compression-ratio bound before exposing content (`crates/wyrd-bundle/src/read.rs`). Verified on dev. | dev |
| B4 bundles | Info disclosure | Secrets in uploaded bundles | Redaction at ingest with a corpus, pseudonymised hosts; verified on dev. Constant-pattern `expect`s in `redact.rs` are justified in the review (F-7). | dev |
| B7 MCP server | Elevation | Agent triggers a rewind without a human | `request_rewind` only stores a pending request. The agent-facing intent socket has no decision query; the decision is `POST /v1/sessions/{sid}/rewind-requests/{id}` behind the daemon token, or `wyrd rewind --confirm`. The MCP tools are read-only except the request. | #209 (request), #218 (confirm), #219 (Studio) |
| B7 MCP server | Tampering | Tool description changes after review ("rug pull") | Blake3 pin per tool at first `tools/list`; a change is recorded as a ledger event (D009). Pins are in memory, so a restart re-pins (F-4). | dev (adapter), #209 (server) |
| B7 MCP server | Info disclosure / injection | Ledger text returned to the agent carries attacker content | Redaction at Standard level, 400-character summaries. Content is still agent-readable history (F-5). | #209 |
| B8 GitHub action | Tampering | Unsigned sensor or CLI release installed | Checksums and cosign keyless verification; `verify-signature: auto` only warns when a release is unsigned (F-8). | dev |
| B8 GitHub action | Injection | Workflow inputs executed as shell | Every input reaches the scripts through `env:`; none is interpolated into `run:`. Verified on dev. | dev |
| B8 GitHub action | Elevation | Action installs the sensor with root | Only `sudo -n` on a runner that already has passwordless sudo; the unit is installed from the action bundle (F-9). | dev |
| B9 Hub CI upload | Spoofing | Forged GitHub OIDC token | Issuer and audience checked; rule must match `repository_owner` and `repository` (exact by default); token org must match. An owner-wide rule needs an audited `wildcard` opt-in, and migrated owner-wide rules are disabled (F-10, #236). | #216, #236 |
| B9 Hub CI upload | Elevation | Workflow from one repo uploads to another org | Trust rules per org; no rule means 403 (`ci-not-trusted`). | #216 |
| B10 Hub deployment | Elevation | Container escape or pod compromise | Non-root, read-only root filesystem, all capabilities dropped, distroless base pinned by digest, deny-all NetworkPolicy with DNS, 443, 5432 and 9000 allowed. Secrets only by `existingSecret`. | #210, #213 |
| B11 Studio offline | Info disclosure | Service worker caches bundles or API answers | Caches only the app shell; never `v1/` or `bundle/` paths (test-enforced). | #214 |
| B12 sensord | Elevation | Compromised sensor daemon with kernel capabilities | Capability bounding set limits the daemon. It does not drop capabilities after attach, and it runs as root (F-1, F-2). #217 changes only drop counting. | dev, #217 |
