# ADR-005: Cell design and user-namespace strategy

- Status: Accepted
- Date: 2026-10-14

## Context

To rewind effects done by any subprocess, the workspace must be
copy-on-write and every change must be attributable to a session. The agent
should see normal paths so tools behave as usual. Ubuntu restricts
unprivileged user namespaces through AppArmor
(`kernel.apparmor_restrict_unprivileged_userns = 1`).

## Decision

- A Cell is: a delegated cgroup v2 scope (`systemd-run --user --scope`), a
  user + mount namespace, an overlay mount with the real workspace as the
  read-only lower layer and a per-branch upper layer, mounted **at the
  workspace's own path** inside the namespace, plus a Landlock ruleset.
- A checkpoint is the content-addressed manifest of the upper layer
  (including overlay whiteouts and opaque directories).
- Rewind and fork never modify a live overlay: the cgroup is frozen, the
  process tree terminated, the mount torn down, the target manifest
  materialised into a fresh upper layer, and the overlay remounted.
- Changes reach the real workspace only through `wyrd apply`, which performs
  a three-way check against the state at session start.
- User-namespace restriction is handled by, in order of preference: an
  AppArmor profile shipped in `deploy/apparmor/` that grants `userns` to the
  `wyrd` binary; or `wyrd-sensord` creating the mount namespace and passing
  it to `wyrdd` by file descriptor.
- `--observe` mode runs without a Cell (no rewind) for environments where
  neither is possible.

## Consequences

- Rewinds are reliable because they never race with writers.
- The agent's own conversation is not rewound; the agent is relaunched with
  its resume/fork options when it has them.
- Writes outside the workspace and network effects are "escaped" effects:
  recorded, flagged irreversible, and denied by default policy.

## Alternatives considered

- **btrfs/ZFS snapshots** — fast, but tie the product to a filesystem.
- **git stash/worktrees** — miss untracked, ignored, and binary files and
  anything outside git's view.
- **Pre-image capture via fanotify** — no sandbox needed, but racy and
  heavy for large writes.
