# Writing a Wyrdline policy

A policy decides what an agent may do in a session. It is a WebAssembly
component implementing the `wyrdline:policy` world
([`policies/wit/policy.wit`](../../policies/wit/policy.wit)), so it can be
written in any language with component tooling; the shipped ones are Rust.

```
wyrd policy build policies/default                 # → policies/default.wasm
wyrd policy test policies/default.wasm policies/default/cases.toml
wyrd run --policy policies/default.wasm -- claude -p "fix the failing cart test"
wyrd policy backtest policies/strict.wasm <session>
```

## The three exports

| Export | Called | Decides |
|---|---|---|
| `kernel-rules(ctx)` | once, when a session starts | rules the kernel enforces for the whole session |
| `on-tool-call(call, ctx)` | before every tool call (agent hook `PreToolUse`, MCP gateway `tools/call`) | `allow`, `deny(reason)`, `ask(reason)` |
| `on-effect(effect, ctx)` | for every kernel effect (exec, sensitive read, write, unlink, rename, connect) | `allow` or a stop |

`ctx` carries the repository root, the task scope globs, and how many
calls were denied so far. Tool-call arguments arrive redacted (secrets
replaced) as JSON.

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
every call gets a fresh instance with a fuel budget (~8 ms) and 32 MiB of
memory; every import other than `log` traps, so a policy has no
filesystem, network, clock, or randomness. A trap, an exhausted budget, or
a malformed result is a deny; a `kernel-rules` that fails stops the
session from starting. `log` keeps 32 lines of 512 bytes per call.

## Writing one in Rust

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
`default` and `strict` policies.

## Testing

`wyrd policy test <policy.wasm> <cases.toml>` runs conformance cases and
exits 3 when one fails:

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

`wyrd policy backtest <policy.wasm> <session | events.jsonl>` replays a
recorded session: tool calls through `on-tool-call`, effects through a
simulation of the kernel rules (connects against the egress rules, using
the recorded DNS name only; sensitive reads; writes that left the Cell;
execs by name) and then `on-effect`. The output lists every event the
policy would have denied or asked about, by `seq`. It is deterministic:
the same policy and session always give the same report. The daemon offers
the same over `POST /v1/policies` and `POST /v1/policies/{id}/backtest`,
and Studio's Backtest tab draws the findings on the river.
