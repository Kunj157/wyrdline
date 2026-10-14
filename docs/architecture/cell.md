# The Cell

How `wyrd run` sandboxes an agent ([ADR-005](../adr/005-cell-design.md)).

## Launch

```
wyrd run (stage one, as the user)
 ├─ create session: ledger, stat baseline of the workspace, branch a
 ├─ serve the session's intent socket and inject agent hooks (see intent.md)
 ├─ choose how to get a mount namespace
 │    observe       → no sandbox
 │    already root  → drop to SUDO_UID/SUDO_GID after mounting
 │    userns works  → unprivileged user + mount namespace
 │    WYRD_PRIVILEGED_HELPER=sudo → stage two via `sudo -n`, drops back to the user
 │    otherwise     → error with the three ways to fix it
 └─ systemd-run --user --scope --unit wyrd-<session>-r<n> -- [sudo -n] wyrd __cell-exec launch.json
        stage two
        ├─ (root mode) join the session cgroup
        ├─ unshare mount namespace (and user namespace when unprivileged)
        ├─ make / rprivate, mount overlay at the workspace's own path:
        │    lowerdir=<workspace>, upperdir=branches/<b>/upper, workdir=branches/<b>/work, userxattr
        ├─ (root mode) setgroups/setgid/setuid to the caller
        ├─ Landlock: writes only beneath the workspace, /tmp, /var/tmp, /dev,
        │    /proc/self and agent state dirs (~/.claude, ~/.codex, ~/.gemini, ~/.config, ~/.cache, ~/.npm)
        └─ exec the agent
```

The `probe` used to decide "userns works" performs the full sequence
(unshare, uid/gid maps, a mount-propagation change). Under Ubuntu's
`apparmor_restrict_unprivileged_userns`, `unshare` alone succeeds but the
namespace has no capabilities.

The overlay exists only inside the agent's mount namespace: the rest of the
system, including the user's editor, keeps seeing the real workspace. The
session data directory is outside the Landlock write set, so the agent
cannot edit its own ledger or checkpoints.

## Checkpoints

A checkpoint is the CAS manifest of the branch's upper layer. One is taken
after every finished tool call reported by the agent's hooks
([intent capture](intent.md)), on `wyrd checkpoint`, and when the agent
exits. While the
agent runs, the session cgroup is frozen (`cgroup.freeze`) for the snapshot
and thawed afterwards. Snapshots are incremental against the branch's
previous checkpoint. A final checkpoint is taken when the agent exits.

## Rewind and fork

1. If the agent runs: freeze, `cgroup.kill`, wait until empty — the
   overlay disappears with the last process of the namespace.
2. Materialise the target manifest into a fresh `branches/<new>/upper`,
   recreating whiteouts (`mknod 0:0`, allowed unprivileged since Linux 5.8)
   and opaque directories (`user.overlay.opaque=y`).
3. Make the new branch current; `--relaunch` starts the agent on it.

Old branches are never modified.

## Apply

`wyrd apply <session> <checkpoint|branch|head>` writes a checkpoint's
changes into the real workspace: files (atomic replace), directories,
symlinks, deletions (whiteouts), and opaque directories (replace). Before
writing, every touched path is compared with the stat baseline taken at
session start; if the user changed it meanwhile, apply refuses (exit 4)
unless `--force`. `--dry-run` lists the operations.

## Privileged mode cleanup

When root mounted the overlay, the kernel leaves a root-owned
`branches/<b>/work/work`. After each run, `wyrd __cell-cleanup` removes it
through the same privilege path (it only accepts `…/branches/<id>/work`).

## Enabling Cell mode on Ubuntu 23.10+

Either install the AppArmor profile (`sudo make apparmor-install
WYRD_BIN=$(which wyrd)`), or opt in to the sudo helper with
`WYRD_PRIVILEGED_HELPER=sudo` (passwordless sudo for `wyrd __cell-exec` and
`wyrd __cell-cleanup` is enough). From Phase 5, `wyrd-sensord` takes over
this role.

## Session states

`preparing → recording → paused` per run; `rewinding` while stopping for a
rewind; `failed` when no namespace could be obtained; `closing → closed`
when `wyrd close` seals the ledger. A session stays open between runs so it
can be rewound, forked, and relaunched.
