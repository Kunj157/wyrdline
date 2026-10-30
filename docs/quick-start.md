# Quick start

Two minutes from an installed `wyrd` ([install](install.md)) to a
recorded, inspected, and exported agent session. Every command below is
run as your own user; nothing here needs root once `wyrd-sensord` is
installed.

## 1. Check the machine (10 s)

```sh
wyrd doctor
```

Example output (an Ubuntu 24.04 workstation; yours will differ):

```
Wyrdline doctor — kernel 6.17.0-14-generic

 ✓ Kernel release                   6.17.0-14-generic
 ✓ BTF type information             /sys/kernel/btf/vmlinux present
 ✓ BPF ring buffer                  BPF ring buffer available (kernel ≥ 5.8)
 ✓ cgroup v2 (unified hierarchy)    /sys/fs/cgroup is cgroup2 [cell]
 ✓ Delegated user cgroup            /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service present (systemd user manager)
 ✓ Landlock LSM                     enabled, ABI v7 [cell]
 ! BPF LSM                          not enabled (active: lockdown,capability,landlock,yama,apparmor,ima,evm)
                                    → Optional. Without it, exec denial is detect-and-stop and file checks rely on Landlock.
 ✓ Unprivileged user namespaces     AppArmor restricts user namespaces; the wyrd profile allows them
 ✓ overlayfs in user namespaces     overlayfs available, mountable in user namespaces [cell]
 ✓ cgroup socket-address hooks      cgroup/connect4 and connect6 hooks available
 ✓ wyrd-sensord                     listening at /run/wyrdline/sensord.sock

 Cell mode: yes   Kernel capture: yes   Kernel network enforcement: yes
```

Lines marked `[cell]` are required for the sandbox; every line that is
not `✓` says what you lose. Exit code 2 means Cell mode is not
available on this machine — see [install](install.md#kernel-requirements)
for the fix, or add `--observe` to `wyrd run` below to record without a
sandbox. `wyrd doctor --json` prints the same report as JSON.

## 2. Record a session (the length of the agent's task)

In the repository you want the agent to work on:

```sh
wyrd run --policy policies/default.wasm -- claude -p "fix the failing cart test"
```

What happens:

- The agent starts inside a **Cell**: its own cgroup, mount and user
  namespaces, an overlay over the workspace, and Landlock rules. Your
  editor keeps seeing the real files; the agent's writes land on the
  overlay.
- For Claude Code, `wyrd run` injects its hooks with `--settings` and
  routes the agent's MCP servers through the gateway; no user or project
  file is changed. Codex CLI and Gemini CLI need a one-time hook
  registration ([adapter coverage](adapter-coverage.md)).
- Every tool call is reported, decided by the policy, and followed by a
  **checkpoint** of the workspace. Every exec, write, rename, delete, and
  connect of the agent's process tree is recorded by the kernel sensor.
- The policy's egress allow-list is enforced in the kernel (`EPERM` on
  `connect`); its file rules by Landlock. When the policy answers `ask`,
  the tool call waits for you: `wyrd ask <session>` in another terminal
  (or Studio) lists and answers it; unanswered after `--ask-timeout`
  (30 s) it is denied.

`wyrd run` prints the session id at start and a summary at the end
(the counts below are placeholders):

```
wyrd: session <id> started
wyrd: policy default b3:<hash>…
…
wyrd: session <id> finished (exit <code>); branch a, <n> checkpoint(s)
wyrd: <n> divergence(s); see `wyrd divergences <id>`
```

Useful variants:

```sh
wyrd run -- claude                                   # no policy: record, checkpoint, agent's own permissions
wyrd run --observe -- claude …                       # no sandbox, no checkpoints: record only
wyrd run --name "cart fix" --workspace ~/shop -- …   # name it; sandbox a directory other than the cwd
wyrd run --deny-tool WebFetch --deny-tool 'mcp__github__*' -- …   # a static deny list, no policy needed
wyrd run --agent fakeagent -- wyrd-fakeagent run demo/scenarios/basic.wyrdscript.toml   # the scripted agent
```

Every session id is a ULID; any unique prefix works in the commands
below. `wyrd sessions` (alias `wyrd ls`) lists them.

## 3. Look at what happened (30 s)

```sh
wyrd show <session>                 # record, event count, chain head, verification, checkpoints
wyrd events <session> --plane effect   # what the kernel saw: exec, write, rename, unlink, connect
wyrd graph <session>                # which tool call caused which effect, with attribution
wyrd explain <session> <seq>        # one effect: its process chain and the call it is attributed to
wyrd divergences <session>          # claims and effects that disagree (rules D001–D010)
wyrd tail <session>                 # follow a session that is still running
```

`wyrd graph` prints one block per tool call. Illustrative output, with
made-up ids and counts:

```
● <tool call id> Bash <command>  [finished]  <n> effect(s)
       <seq>  direct    exec <program> <args>
       <seq>  direct    write <path>
○ not caused by a single tool call: <n>
       <seq>  ambiguous     write <path> (one of <call id>, <call id>)
<n> call(s), <n> effect(s): ambiguous <n>, direct <n>, window <n>
```

`--plane` takes `meta`, `intent`, `effect`, `state`, `guard`, or
`verdict`; `--json` on any of these prints one JSON object per line or
the whole structure, for scripting.

**Studio.** Start the daemon once and open the browser UI:

```sh
wyrdd &                             # unix socket for your UID + 127.0.0.1:7717 with a token
wyrd studio                         # opens Studio with the token in the URL fragment
```

The session screen shows the Causal River (intent above, effects below,
threads between them, a knot per checkpoint), an inspector for every
event with its attribution and policy decision, the events table, the
transcript, the checkpoints and branches with rewind, fork, diff, and
apply, the session's divergences, and a backtest tab; live sessions
stream in as they run. The Divergences page is the inbox across
sessions, where findings are acknowledged or marked false positives.

## 4. Rewind, fork, apply (20 s)

```sh
wyrd checkpoints <session>          # c1, c2, … per branch
wyrd rewind <session> c3            # new branch from checkpoint 3; --relaunch starts the agent on it
wyrd fork <session> c3 --name b     # the same, with a name
wyrd diff <session> c3 head         # + added, - removed, ~ modified between two checkpoints or branches
wyrd apply <session> head --dry-run # what would change in the real workspace
wyrd apply <session> head           # write it; refuses (exit 4) if you changed those files meanwhile, --force overrides
```

Old branches are never modified; `apply` is the only command that
touches the real working tree.

A rewind cannot take back network requests the agent made after the
checkpoint. When there are any, `wyrd rewind` prints a warning on stderr
with one line per request (its seq, address, port and verdict), and the
`state.checkpoint.rewound` event lists the same seqs in
`irreversible_seqs`. Nothing is printed when the list is empty.

An agent connected through `wyrd mcp` can ask for a rewind but cannot do
one itself. The request is recorded in the ledger and waits for you:

```sh
wyrd rewind <session> --requests              # the agent's pending requests
wyrd rewind <session> --confirm <id> --relaunch   # rewind; the request id is the event's seq
wyrd rewind <session> --reject <id> --note "…"    # or decline it
```

The same confirm and reject actions are in Studio's session view.

## 5. Seal, verify, export (10 s)

```sh
wyrd close <session>                # seal the ledger: sign (session id, head seq, head hash) with the host key
wyrd verify <session>               # recompute every hash and the signature; exit 3 on the first bad seq
wyrd export <session> -o session.wyrd   # signed bundle, strict redaction by default
```

The bundle holds the redacted events, the checkpoint content, claims,
and divergences, in one zstd-compressed archive signed with the same
host key ([bundle format](bundle-format.md)). `--redact standard` keeps
user and host names; `--no-content` leaves out file content for
metadata-only sharing.

Studio replays a session with no daemon and no network, the correlator
running as WebAssembly in the browser: today from an events export
(`wyrd events <session> --json > session.jsonl`, opened with
`?offline=<url>` on the Studio origin, which is how the
[website demo](https://kunj157.github.io/wyrdline/) works), or a signed
`.wyrd` bundle: `wyrd open session.wyrd` opens it in Studio without the
daemon, and Studio's file picker or drag-and-drop does the same. Bundles
are verified before anything is shown.

## 6. Try it with the demo instead

`demo/victim-shop` is a small web shop with a failing cart test and a
planted prompt injection in its `CONTRIBUTING.md`;
`demo/scenarios/injection.wyrdscript.toml` is the scripted agent that
falls for it. `make demo` runs the whole scenario (sensor, Cell, default
policy) and asserts on the ledger, so you can see every piece above in
one go without an agent subscription: the denied `connect`, the
divergences, the rewind.

What next:

- [Writing a policy](policies/README.md): `kernel-rules`, `on-tool-call`,
  `on-effect`, testing and backtesting a policy against a recorded session.
- [Adapter coverage](adapter-coverage.md): what is captured per agent.
- [Limitations](limitations.md) and the [FAQ](faq.md).
