# Scenarios

Scripts for `wyrd-fakeagent`, the deterministic agent that drives the
whole capture pipeline in tests (plan §21.4):

```
wyrd run --agent fakeagent -- wyrd-fakeagent run demo/scenarios/basic.wyrdscript.toml
```

The fake agent reports every step through the real `wyrd hook claude-code`
command, runs the scripted tool calls for real (shell commands, file
writes and edits), talks to MCP servers through `wyrd mcp-proxy`, and
prints a transcript. Outside `wyrd run` (no `WYRD_BIN`) it simply plays
the script.

## Format

```toml
name = "basic"
description = "What the scenario exercises."
exit_code = 0                         # optional

[mcp.toy]                             # MCP servers by name
command = ["wyrd-fakeagent", "serve-mcp", "--rug-pull-after", "1"]

[[turn]]
prompt = "Fix the typo. Token: ${WYRD_SCENARIO_SECRET}"   # ${VAR} expands from the environment
reply = ["Intermediate text.", "Final message."]

[[turn.step]]
tool = "Bash"                         # Bash {command} · Write {file_path, content}
input = { command = "make test" }     # Edit {file_path, old_string, new_string} · Read {file_path}
                                      # any other tool: succeeds, does nothing
[[turn.step]]
mcp = "toy"                           # an MCP tool call (agent tool name mcp__toy__echo)
tool = "echo"
input = { text = "ping" }

[[turn.step]]
mcp = "toy"
list = true                           # tools/list
```

Call ids are `fa-<turn>-<step>`, so tests can name calls exactly. A
denied call is skipped, like a real agent does.

`wyrd-fakeagent serve-mcp` is a toy stdio MCP server with `echo` and
`touch` tools; `--rug-pull-after N` swaps `echo`'s description for a
malicious one after N listings.

## Tests

`tests/scenarios/main.rs` runs each scenario under `wyrd run` and asserts
on the ledger: `make test-scenarios` (observe mode, unprivileged) and
`make test-integration` (real Cell, through the sudo helper).
