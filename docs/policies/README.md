# Writing a Wyrdline policy

A policy decides what an agent may do in a session. It is a WebAssembly
component implementing the `wyrdline:policy` world
([`policies/wit/policy.wit`](../../policies/wit/policy.wit), package
`wyrdline:policy@0.1.0`), so it can be written in any language with
component tooling; the shipped ones are Rust.

```
wyrd policy build policies/default                 # → policies/default.wasm
wyrd policy test policies/default.wasm policies/default/cases.toml
wyrd run --policy policies/default.wasm -- claude -p "fix the failing cart test"
wyrd policy backtest policies/strict.wasm <session>
```

Without a policy, `wyrd run` still sandboxes, records, and checkpoints;
the agent's own permission prompts apply unchanged, and
`--deny-tool <name>` (exact, or a prefix ending in `*`) adds a static
deny list. A policy adds decisions per tool call, `ask` prompts, kernel
file and egress rules for the whole session, and effect-time stops.

## The shipped policies

Both are built from the same rules ([`policies/common`](../../policies/common/src/lib.rs))
and differ in how far they go:

| | `policies/default` | `policies/strict` |
|---|---|---|
| Risky actions | **ask** a human | **deny** |
| `fs-read-deny` (kernel) | `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.gnupg` | the same plus `~/.netrc`, `~/.docker`, `~/.kube`, `~/.npmrc`, `~/.pypirc` |
| Egress (kernel) | loopback, `api.anthropic.com`, the package registries (npm, PyPI, crates.io, Go proxy) and GitHub hosts | loopback and `api.anthropic.com` only |
| `exec-deny` (effect time) | none | `curl`, `wget`, `nc`, `ncat`, `socat`, `telnet` |
| Writes | inside the workspace only; secret-classified paths are risky | the same, and outside the task scope (`scope-globs`) is denied |
| Reads (`Read`, `Grep`, `Glob`, `NotebookRead`) | secret paths are risky (`.ssh`, `.aws`, `.gnupg`, `.config/gh`, `.env*` except `.env.example`) | the same plus `.netrc`, `.npmrc`, `.pypirc`, `.docker`, `.kube`, `*.pem`, `*.key`, `*.p12` |
| `Bash` | `git push --force` and `rm -r` of `/`, `~`, `$HOME`, `/home`, `/etc`, `/usr` are denied; `curl`/`wget` to a host off the allow-list (or without a URL) and any argument naming a secret path are risky | the same, and the `exec-deny` programs are denied as commands |
| `WebFetch` | a host off the allow-list is risky | the same, with the smaller list |
| `on-effect` | always allow (the default policy decides before the call and in the kernel) | stops the session on a sensitive read of a secret path or an exec of a denied program |
| Any other tool | allow | allow |

Build them with `make policies` (or `wyrd policy build policies/default`
and `…/strict`); their conformance cases are `policies/default/cases.toml`
and `policies/strict/cases.toml` (`make test-policies`).

## The three exports

| Export | Called | Decides |
|---|---|---|
| `kernel-rules(ctx)` | once, when a session starts | rules the kernel enforces for the whole session |
| `on-tool-call(call, ctx)` | before every tool call (agent hook `PreToolUse`, MCP gateway `tools/call`) | `allow`, `deny(reason)`, `ask(reason)` |
| `on-effect(effect, ctx)` | for every kernel effect (exec, sensitive read, write, unlink, rename, connect) | `allow` or a stop |

`ctx` carries the repository root, the task scope globs, and how many
calls were denied so far. Tool-call arguments arrive redacted (secrets
replaced) as JSON; `tool` is the name the agent uses (`Bash`, `Edit`,
`mcp__github__create_issue`), `adapter` is `claude-code`, `codex`,
`gemini-cli`, `mcp`, or `fakeagent`. An effect has a `kind` of `exec`,
`file.open_write`, `file.read_sensitive`, `file.unlink`, `file.rename`,
or `net.connect`, with `path`, `host` and `port`, or `exe` filled as
applicable.

`scope-globs` is filled from conformance cases (`[ctx] scope_globs`);
`wyrd run` and `wyrd policy backtest` currently pass an empty list, so a
rule that depends on it (the strict policy's "outside the task scope")
does not fire in live sessions yet. `.wyrdline/scope.toml` is read by
the divergence rules (D002), not by the policy host.

### `kernel-rules`

```
fs-read-deny        paths the agent may not read (`~` allowed)
fs-write-allow      paths it may write besides the workspace and the Cell defaults
exec-deny           programs that may not run
egress-allow-cidrs  destinations the agent may connect to
egress-allow-hosts  host names, resolved by `wyrd run` (and again every minute)
egress-allow-ports  ports; empty means every port
```

Listing any egress rule restricts egress to the listed destinations.
Patterns such as `**/.env*` cannot be enforced by the kernel and are
reported as "left to tool-call and effect-time checks": cover them in
`on-tool-call`.

### `ask`

An `ask` holds the tool call until a human answers it with `wyrd ask
<session>` or in Studio's session screen; with no answer within
`--ask-timeout` (30 s) the call is denied. From `on-effect`, `ask` is a
deny: an effect has already happened and cannot wait.

```
wyrd ask <session>                 # interactive: answer asks as they arrive, until the session ends
wyrd ask <session> --json          # list the pending asks and exit
wyrd ask <session> --allow 3       # answer one by id
wyrd ask <session> --deny 3
```

Each pending ask shows its id, the tool and its arguments, the policy's
reason, and how many seconds are left. The answer is recorded as a
second `guard.decision` naming who answered (`cli` or Studio) and how
long it took. Answers travel over the session's intent socket with a
control token the Cell cannot read, so the agent cannot answer for you.

In the MCP gateway (`tools/call`), `ask` is treated as deny.

## What each layer enforces

| Layer | Enforced by | When | Can it be bypassed by the agent? |
|---|---|---|---|
| `on-tool-call` | agent hook, MCP gateway | before the call | Only if the agent does not report the call (its own hooks); the kernel layers still apply |
| `fs-read-deny`, `fs-write-allow`, ports | Landlock | at the syscall, before | No: applies to the agent's whole process tree |
| egress | cgroup `connect4/6` programs (`wyrd-sensord`) | at `connect`, before (`EPERM`) | No, for TCP and connected UDP; addresses, not names |
| `exec-deny`, `on-effect` | the effect stream | after the fact: **detect and stop** | The effect has happened; the agent is frozen and killed |

Limits to keep in mind:

- Host names become IP addresses: a CDN address shared with other names is
  allowed for all of them.
- Read denial is enforced as Landlock's complement: names inside a denied
  directory can still be listed, and files created later next to a denied
  path are not readable.
- An effect-time deny stops the session; it cannot undo the effect.
- Without `wyrd-sensord`, egress and effect checks do not run; the session
  is marked Degraded and says so.

## The sandbox a policy runs in

Policies are untrusted code ([ADR-007](../adr/007-wasm-policy-host.md)):
every call gets a fresh instance with a fixed fuel budget (200 million
units per call, `FUEL_PER_CALL` in `wyrd-policy`) and 32 MiB of memory; every import other than `log` traps, so a policy has no
filesystem, network, clock, or randomness. A trap, an exhausted budget, or
a malformed result is a deny; a `kernel-rules` that fails stops the
session from starting. `log` keeps 32 lines of 512 bytes per call.

## Writing one in Rust

A policy crate is an ordinary `cdylib` built for `wasm32-wasip2`
(`rustup target add wasm32-wasip2`; the pinned toolchain in
[`rust-toolchain.toml`](../../rust-toolchain.toml) already has it).
`wyrd policy build <dir> [-o FILE]` runs
`cargo build --release --target wasm32-wasip2` on the crate, copies the
component next to the directory as `<dir>.wasm`, loads it once to check
that it implements the world, and prints its version: `b3:` followed by
the first 16 hex digits of the component's BLAKE3, which is what every
`guard.decision` records as `policy_version`. The policy id is the file
stem (`default` for `policies/default.wasm`).

```toml
# policies/mine/Cargo.toml
[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = { version = "0.63.0", default-features = false, features = ["macros", "realloc"] }
```

```rust
wit_bindgen::generate!({ path: "../wit", world: "policy" });

struct Mine;

impl Guest for Mine {
    fn kernel_rules(_: SessionCtx) -> KernelRules {
        KernelRules {
            fs_read_deny: vec!["~/.ssh".into()],
            fs_write_allow: vec![],
            exec_deny: vec!["curl".into()],
            egress_allow_cidrs: vec!["127.0.0.0/8".into()],
            egress_allow_hosts: vec!["api.anthropic.com".into()],
            egress_allow_ports: vec![],
        }
    }
    fn on_tool_call(call: ToolCall, _: SessionCtx) -> Decision {
        if call.tool == "WebFetch" {
            Decision::Ask("web access".into())
        } else {
            Decision::Allow
        }
    }
    fn on_effect(_: Effect, _: SessionCtx) -> Decision {
        Decision::Allow
    }
}

export!(Mine);
```

Keep the rules in plain functions you can unit-test natively, as
[`policies/common`](../../policies/common/src/lib.rs) does for the shipped
`default` and `strict` policies. Things to know while writing:

- The only import is `log`; anything else the toolchain links (printing,
  the clock, environment variables, files, sockets) traps, and a trap is
  a deny. Decide from the arguments alone.
- Paths are strings as the agent and the kernel report them. `~` is
  accepted in `kernel-rules` and expanded by the host; inside
  `on-tool-call` recognise home-directory paths by shape (`/.ssh/`),
  since the policy cannot look up `$HOME`.
- `deny` and `ask` carry a reason: it is shown to the human who answers
  an ask, returned to the agent with a denial, and stored in the ledger.
- A component larger than 16 MiB is refused; a build with
  `--release` of the shipped policies is well under 1 MiB.
- [`policies/examples/hostile`](../../policies/examples/hostile/src/lib.rs)
  misbehaves on request (spins, allocates, traps) and is what the host's
  limit tests run; [`policies/examples/escape`](../../policies/examples/escape)
  is the fixture of the escape suite.

## Testing

`wyrd policy test <policy.wasm> <cases.toml>` runs conformance cases and
exits 3 when one fails. A case is a `tool_call` or an `effect`, the
decision expected (`allow`, `deny`, `ask`), and optionally a substring
the reason must contain; `[ctx]` sets `repo_root`, `scope_globs`, and
`prior_denials` for every case, and the optional `[kernel_rules]` lists
entries each of the policy's kernel rules must contain:

```toml
[ctx]
repo_root = "/work/shop"

[kernel_rules]                  # lists the policy's rules must contain
fs_read_deny = ["~/.ssh"]

[[case]]
name = "curl to an unknown host asks"
tool_call = { tool = "Bash", args_json = '{"command":"curl https://evil.example"}' }
expect = "ask"
reason_contains = "evil.example"

[[case]]
name = "running curl stops the session"
effect = { kind = "exec", exe = "/usr/bin/curl" }
expect = "deny"
```

## Backtesting

`wyrd policy backtest <policy.wasm> <session | bundle.wyrd | events.jsonl> [--json]`
replays a recorded session (a `.wyrd` bundle is verified first; one that
does not verify is refused with exit 3): tool calls through `on-tool-call`, effects
through a simulation of the kernel rules (connects against the egress
rules, using the recorded DNS name only; sensitive reads; writes that
left the Cell; execs by name) and then `on-effect`. The output lists
every event the policy would have denied or asked about, by `seq`:

```
strict b3:9a1c… over 812 events: 3 finding(s)
  seq 118    hook    deny reads an environment file (/work/shop/.env)
  seq 131    kernel  deny connect to 203.0.113.9:443 is not on the egress allow-list
  seq 133    effect  deny executed curl
  note: fs-read-deny pattern **/.env* cannot be expressed in Landlock; left to tool-call and effect-time checks
```

The stage is `hook`, `gateway`, `kernel`, or `effect`; notes list what
the kernel could not have enforced, or why the policy gave no kernel
rules (such a session would not start). The events file form takes the
output of `wyrd events <session> --json`, so a session exported from
another machine can be backtested too. It is deterministic: the same
policy and session always give the same report.

The daemon offers the same for Studio: `GET /v1/policies` lists the
uploaded components, `POST /v1/policies?name=<id>` stores one (the body
is the `.wasm`; a component that does not implement the world is
refused with 422), and `POST /v1/policies/{id}/backtest` with
`{ "session_id": … }` returns the report. Studio's Backtest tab on the
session screen uploads a policy and draws the findings on the river.
In bundle mode, where there is no policy host in the browser, Studio's Backtest tab
loads the `--json` report file instead.

## Project configuration (`.wyrdline/`)

Three optional files in the workspace, read by the divergence rules
when `wyrd run` finishes and by `wyrd divergences`
([limitations](../limitations.md)); a malformed file is an error, and
unknown keys are rejected:

```toml
# .wyrdline/config.toml — every list extends the built-in defaults
[divergence]
test_runners = ['^just\s+test']        # regexes over each shell segment, program reduced to its file name (D001, D005)
build_runners = ['^just\s+build']
build_dirs = ["generated"]              # directories whose unlinks are not mass destruction (D008)
known_hosts = ["artifacts.example"]     # hosts that are expected without being mentioned (D007)
test_files = ["spec/**"]                # globs of test files (D006)
egress_window_ms = 10000                # D003: a connect within this long after a secret read
mass_unlink_threshold = 20              # D008
```

```toml
# .wyrdline/scope.toml — globs the agent is expected to touch (D002)
include = ["src/**", "tests/**"]
```

```toml
# .wyrdline/secrets.toml — extra secret path patterns (never secret values)
paths = ["config/credentials.yml"]
```
