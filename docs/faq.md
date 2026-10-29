# FAQ

### What problem does this solve?

Coding agents run real commands on real machines. What the agent *says*
it did (its transcript and tool calls) and what *actually* happened
(kernel effects: processes, file writes, network connections) can
differ, and the agent's own undo covers only its own edit tool.
Wyrdline records both, links each effect to the tool call that caused
it, lets you rewind the workspace to any point, enforces a policy in
the kernel, and flags the disagreements.

### Do I need root?

Only to install `wyrd-sensord`, which runs as a systemd service with a
bounded capability set and no network. `wyrd run`, `wyrdd`, Studio, and
every inspection command run as your user. On Ubuntu 23.10 and later,
unprivileged user namespaces need either the shipped AppArmor profile
(one `sudo make apparmor-install`) or `WYRD_PRIVILEGED_HELPER=sudo`
([install](install.md#kernel-requirements)).

### Does it work without the sensor?

Yes, with less. Tool calls, MCP traffic, policy decisions, and
checkpoints are recorded and rewind works; kernel effects are missing,
the egress allow-list is not enforced, and the session is marked
Degraded so nobody mistakes it for a complete record
([limitations](limitations.md)).

### Which agents are supported?

Claude Code is wired automatically by `wyrd run` (hooks and MCP
servers, without touching your settings). Codex CLI and Gemini CLI use
the same `wyrd hook <adapter>` command after a one-time registration
in their hook configuration. Any MCP client is covered through
`wyrd mcp-proxy`, and any agent that emits OpenTelemetry GenAI spans
through OTLP. Kernel effects are recorded for every agent whatever it
reports. Details and verification status per adapter:
[adapter coverage](adapter-coverage.md).

### Does `wyrd run` change my agent's configuration?

No. For Claude Code it writes a settings file and an MCP configuration
into the session directory and passes them on the command line
(`--settings`, `--mcp-config … --strict-mcp-config`); the ledger keeps
the command as you typed it. For Codex and Gemini it does not inject
anything, precisely because that would mean editing your files; it
prints how to register the hook instead.

### Can the agent see or change the recording?

The agent runs inside the Cell; the session directory (ledger,
checkpoints, control token) and the daemon socket are outside its
Landlock write set and read set. It can *report* through the intent
socket, and its reports are treated as untrusted: kernel effects are
the ground truth they are compared against. The socket accepts ingest
only; it cannot start, stop, rewind, or apply anything. A rewind the
agent asks for (`meta.rewind.requested`) is only a request until a
human confirms it.

### What exactly is recorded in the kernel?

For the agent's process tree (every process in the session's cgroup or
a cgroup below it): `exec`, process creation and exit, file opens for
writing, reads of
secret-looking paths only, `unlink`/`rmdir`, `rename`, `mkdir`,
`chmod`, `truncate`, and every `connect` with its verdict. Programs
report successful calls at syscall exit. Everything else on the machine
is invisible to the sensor ([sensor](architecture/sensor.md)).

### How much does capture cost?

Under 2 % on a `cargo build` with the sensor capturing (+0.8 % on the
workstation, +1.5 % on a hosted runner), and 0.2–1.2 % for every other
process on the machine while the sensor is loaded; a syscall-dense
stress workload costs 6–9 %. The sensor sustained 196 692 events/s with
zero drops. Numbers and method:
[sensor performance](results/perf-sensor.md). A denied connect adds
nothing measurable (2.8 µs at p99, less than an allowed one):
[enforcement](results/enforcement.md).

### Where does my data go?

Nowhere, unless you send it. Sessions live in your data directory
(`~/.local/share/wyrdline` by default). The daemon listens on a unix
socket for your UID and on loopback with a bearer token. Bundles are
written to the file you name. The Hub and the GitHub Action upload only
what you point them at.

### What is sent to an LLM?

Only when `ANTHROPIC_API_KEY` is set (or `LLM_PROVIDER=anthropic`):
redacted assistant messages and a list of tool-call names with short
arguments, so a model can extract *claims* ("all tests pass") as
structured output. Never file contents, never secrets, never raw
effects. Every claim's quote must be a verbatim substring of the
transcript or it is dropped; whether a claim holds is decided by the
rules in code. Spend is capped per session (`WYRD_LLM_MAX_COST_USD`,
0.50 by default). Without a key the regex extractor and all ten rules
still run.

### Can I use it without a policy?

Yes: `wyrd run -- <agent>` records, checkpoints, and lets you rewind,
with the agent's own permission prompts untouched. `--deny-tool` adds a
static deny list without writing a policy. `--policy` adds decisions
per tool call, `ask` prompts, kernel file and egress rules, and
effect-time stops. The shipped `policies/default.wasm` is the sensible
start: writes only inside the workspace, asks before reading secrets or
fetching unknown hosts, denies force-pushes and `rm -rf`-shaped
commands, egress only to package registries, GitHub, and the model API
([policy guide](policies/README.md)).

### The policy asked something and the agent is stuck. What now?

`wyrd ask <session>` in another terminal lists pending asks and answers
them (`--allow ID` / `--deny ID`, or interactively); Studio's session
screen shows the same dialog. Unanswered asks are denied after
`--ask-timeout` seconds (30 by default), and the agent continues with
the denial and its reason.

### Rewind restored the files but the agent "remembers" what it did.

Expected: rewind restores the workspace, not the conversation. Start
the next attempt with `--relaunch` and a scoped prompt, or use the
agent's own resume flags in the relaunched command
([limitations](limitations.md)).

### Something failed: which exit code means what?

`0` ok · `1` usage or an error · `2` capability missing or session not
found · `3` verification failed (`wyrd verify`, `wyrd policy test`) ·
`4` conflict (`wyrd apply` over files you changed) · `5` daemon
unavailable (`wyrd daemon status`, `wyrd studio`).

### How do I know a session or bundle was not tampered with?

`wyrd verify <session>` recomputes the BLAKE3 chain over every event
and checks the Ed25519 seal; it names the first bad `seq`. A bundle is
verified the same way (archive, signature, manifest, every file hash,
the chain) before any of its content is read, also in the browser.
Whoever holds the host key can re-sign a rewritten history, so this is
evidence against everyone else, not against the key holder
([ledger format](ledger-format.md), [bundle format](bundle-format.md)).

### Can I share a session with someone who has no Wyrdline installed?

Export it (`wyrd export <session> -o x.wyrd`, strict redaction by
default) and give them the file. Any `wyrd` verifies it
([bundle format](bundle-format.md)); Studio opens the bundle
offline (`wyrd open`, or its file picker and drag-and-drop), as well as
an events export. The
Hub (Phase 12, in progress) is the team server for bundles: roles,
search, and shared annotations.

### Does it run in CI?

A GitHub Action is being assembled in `integrations/github-action`
(Phase 13): its installer downloads a release and verifies the checksum
and the cosign signature, its sensor script installs and starts
`wyrd-sensord` on the runner and refuses to run the agent unprotected
unless told otherwise, and its reporter renders the divergences as a
job summary and one pull-request comment. The composite action that
ties them around `wyrd run` is in place (#160); the Hub upload through
the job's OIDC token (no secret to store) is open as #161.

### What does it not do?

[Limitations](limitations.md): no macOS or Windows, no TLS interception,
no conversation rewind, no undo for network effects, address-based
(not name-based) egress rules, detect-and-stop for forbidden programs,
and heuristics with confidence levels rather than certainty for
attribution and divergences.

### How do I report a vulnerability?

Privately, through GitHub security advisories on the repository, not in
a public issue ([CONTRIBUTING](../CONTRIBUTING.md)).
