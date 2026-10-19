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
        ├─ Landlock: no reads of Wyrdline's data (other sessions, CAS, keys, this
        │    session's ledger and control token) or state dirs, nor of the policy's fs-read-deny;
        │    TCP connects only to the policy's ports, if it lists any
        ├─ wait at the start gate (FIFO in the session dir)
        └─ exec the agent
```

**Start gate.** Stage one waits until stage two has finished all setup
and blocks at the gate, then registers the session cgroup with
`wyrd-sensord` (which acknowledges only once the cgroup is in the kernel
map), and only then releases the gate. The ledger therefore contains the
agent's effects from its very first syscall and none of Wyrdline's own
setup. If no sensor is available, the session is marked Degraded with
`meta.sensor.lost` and the agent still runs; a lost connection mid-run is
recorded the same way, and a reconnection with `meta.sensor.resumed`.

The `probe` used to decide "userns works" performs the full sequence
(unshare, uid/gid maps, a mount-propagation change). Under Ubuntu's
`apparmor_restrict_unprivileged_userns`, `unshare` alone succeeds but the
namespace has no capabilities.

The overlay exists only inside the agent's mount namespace: the rest of the
system, including the user's editor, keeps seeing the real workspace. The
session data directory is outside the Landlock write set, so the agent
cannot edit its own ledger or checkpoints.

## Policy kernel rules

With `wyrd run --policy`, the policy's `kernel-rules` are compiled by
`wyrd-policy::kernel` and recorded as a kernel-stage `guard.decision`:

- `fs-write-allow` joins the write allow-list above;
- `fs-read-deny` (`~` expanded, relative paths under the workspace) is
  enforced as a complement, because Landlock only grants: every sibling
  along the path to a denied entry gets read rights, the ancestors only
  directory listing. Names inside a denied directory can still be listed;
  entries created later inside an ancestor are not readable;
- `egress-allow-ports` become Landlock TCP connect rules (ABI ≥ 4);
- patterns such as `**/.env*` cannot be expressed and are left to
  tool-call and effect-time checks (listed in the decision's reason).

A policy whose `kernel-rules` fails cannot start a session. When the
kernel's Landlock ABI cannot enforce a requested right (or in observe
mode), the session is marked Degraded with the reason.

## Effect-time guard

While the agent runs, every kernel effect is also checked:

- the exec deny list (`exec-deny`): an exec whose program name is on the
  list, or whose binary has the same size and BLAKE3 hash as a listed
  program found on `PATH` at session start (a renamed copy), is denied;
- the policy's `on-effect` for execs, sensitive reads, writes, unlinks,
  renames, and connects (`ask` counts as deny: an effect cannot wait).

A deny is recorded as an effect-stage `guard.decision` citing the
effect's `seq`, and in Cell mode the agent is stopped (cgroup frozen and
killed). This is **detect-and-stop**: the effect has already happened
and cannot be undone; the forbidden program may run briefly. Network and
filesystem access are enforced before the fact by the cgroup connect
programs and Landlock. Observe mode records the decision without
stopping.

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
