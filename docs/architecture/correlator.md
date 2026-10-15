# The correlator

`wyrd-correlator` answers one question for every kernel effect of a
session: **which tool call caused it, and how sure are we?** It is a pure
function of the ledger's events (no I/O, clocks, or threads), deterministic,
and compiled both natively (daemon, CLI) and to WebAssembly (`wyrd-wasm`,
the Studio's offline mode). CI checks that both builds produce
byte-identical graphs (`make wasm-parity`).

## Output

```
Graph { calls, processes, effects, agent_root }
Call    { call_id, tool, source, window [start_ns, end_ns], command?, target_path?, nested_in?, effects[] }
Process { (pid, start_ns), ppid, exe, argv, call_id?, attribution }
Effect  { seq, kind, pid, call_id?, candidates[], attribution, confidence_bp }
```

## Attribution rules

| Attribution | Confidence | When |
|---|---|---|
| `direct` | 1.0 | The process (or an ancestor) is the exec of a command call matched by its command line — `sh -c <cmd>` or a direct exec, prefix match for the 63 bytes the kernel keeps per argument — or the effect touches the target path of an edit/write call inside its window. Whole subtrees inherit, including background processes that outlive the call. |
| `window` | 0.8 | A process the agent forks while exactly one command call is open owns its subtree when that call's exec cannot be matched (agents wrap commands: Claude Code runs `bash -c -l "source …snapshot…; eval '<cmd>'"`, whose first 63 bytes never contain the command). Otherwise: an effect of the agent's process tree inside exactly one open call window. |
| `ambiguous` | 0.5 / n | Inside several open windows without a direct link: candidates are listed, **none is picked**. |
| `agent_runtime` | — | The agent process itself, outside every window (its own state files, telemetry). |
| `infrastructure` | — | Wyrdline's own hook helpers (`wyrd hook …`) the agent spawns around tool calls, and their forks. |
| `unattributed` | — | Anything else: processes outside the agent tree, or the tree outside any window. |

Gateway (MCP) calls nested inside the hook call of an `mcp__…` tool are
the same call seen twice; they attach to the smallest enclosing hook call
(ties: earliest) and never compete with it. Processes are keyed by
`(pid, start_ns)`, so a reused PID is a different process.

## Evaluation

See [`docs/results/attribution.md`](../results/attribution.md).
