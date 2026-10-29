# Adapter coverage

What Wyrdline captures for each kind of agent, through which path, and
how far each path has been verified. The wiring details (hook
registration, decision formats) are in
[agent adapters](architecture/adapters.md) and
[intent capture](architecture/intent.md); this page is the coverage
matrix.

Two things hold for every agent, whatever it reports:

- **Kernel effects** are captured by `wyrd-sensord` for the whole
  process tree of the session: exec, process creation and exit, opens
  for writing, reads of secret-looking paths, unlink, rename, mkdir,
  chmod, truncate, and every `connect` with its verdict
  ([sensor](architecture/sensor.md)). This is the ground truth; the
  intent paths below only add *what the agent said*.
- **Checkpoints** are taken after every tool call the agent reports;
  without reported calls, only `wyrd checkpoint` and the final
  checkpoint at exit exist.

## Intent paths

| Path | Agent | Wired by | Captured | Not captured | Decisions | Verified |
|---|---|---|---|---|---|---|
| Hooks: `wyrd hook claude-code` | Claude Code | `wyrd run`, automatically (`--settings` with every hook; no user or project file changes) | prompts (`UserPromptSubmit`), every tool call before and after (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`: tool, redacted input, status, exit code, duration, short result), denials, assistant messages per turn (`Stop`, `SubagentStop`: transcript texts plus `last_assistant_message`) | `SessionStart`, `SessionEnd`, and other hooks (ignored); the model traffic | `deny` and `ask` with a reason through `hookSpecificOutput.permissionDecision`; `allow` prints nothing, so the agent's own permission rules still apply | recorded Claude Code 2.1 payloads are the fixtures; real runs in a Cell; the fakeagent speaks this adapter in every scenario test |
| Hooks: `wyrd hook codex` | Codex CLI | you, once: register the command for `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop` in Codex's hook configuration ([`integrations/codex`](../integrations/codex/README.md)) | the same as Claude Code: Codex uses the same payload and decision format | as above | as above | field names checked against Codex 0.160; **no live hook run yet** |
| Hooks: `wyrd hook gemini-cli` | Gemini CLI | you, once: merge [`integrations/gemini-cli/settings.json`](../integrations/gemini-cli/settings.json) into `.gemini/settings.json` or your user settings | prompts (`BeforeAgent`), tool calls (`BeforeTool`/`AfterTool`, paired by a hash of tool and input), the final message (`AfterAgent`) | exit codes and durations of calls | `deny` with a reason as `{"decision": "deny", "reason": …}`; `ask` is treated as deny | built from the documented contract; **not verified live** (not installed on the development machine) |
| MCP gateway: `wyrd mcp-proxy --server <name> -- <cmd>` | any MCP client | `wyrd run` for Claude Code (every stdio server in its user config, `.mcp.json`, and local project entry is rewritten through the proxy; Streamable HTTP servers too, with `--remote`); by hand for other clients: wrap each server command | every request and response with an id (method, redacted params, short result), `tools/call` as a tool call (`mcp__<server>__<tool>`), decisions at the gateway stage, tool description and schema pinning (a later change is flagged: D009); the pins are kept in the session's `mcp-pins.json`, so they survive a gateway restart (#233) | notifications; the legacy HTTP+SSE transport (`"type": "sse"`, passed through unproxied); server-initiated messages on a standalone GET stream; pins are per session, so a change between two separate `wyrd run`s is not flagged | a denied `tools/call` is answered with JSON-RPC error `-32001` and never forwarded; `ask` is treated as deny in the gateway | gateway end-to-end tests; fakeagent scenarios with a toy server, including the rug-pull case |
| OTLP: `POST /v1/sessions/{sid}/otlp/v1/traces` on `wyrdd` | any agent emitting OpenTelemetry GenAI spans | `wyrd run` sets `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `…_PROTOCOL=http/json`, `…_HEADERS` for the agent (with a token that opens only that session's ingest routes) unless you set an OTLP endpoint yourself; needs a running `wyrdd` | `execute_tool` spans as tool calls (started and finished, source `otlp`) | model-call spans (`chat`, `text_completion`, `invoke_agent`, …): counted, not recorded; protobuf bodies (415, JSON only); at most 10 000 spans per request | none: OTLP arrives after the fact | mapping unit tests, daemon ingest tests, `wyrd run` + `wyrdd` end to end with a scripted exporter |
| Session tools: `wyrd mcp` | any MCP client, Claude Code included | you register it as a stdio server (`{"mcpServers": {"wyrdline": {"command": "wyrd", "args": ["mcp"]}}}`); `wyrd run` sets `WYRD_SESSION_ID` and `WYRD_INTENT_SOCKET` in a Cell, which binds the server to that session | the agent's calls to the server are recorded by the path the agent uses for its tool calls (hooks, or the gateway if the server is wrapped) | the server's answers are quoted data, cut at 200 items and 32 KiB per block (1000 characters per string) (#235). | The agent cannot rewind, apply, or change policy. Its one write is `request_rewind`, which records `meta.rewind.requested` and waits for a person (`wyrd rewind … --confirm` or `--reject`, or Studio; #218 and #219) | `tools/list --strict` under the MCP inspector and the protocol tests in `crates/wyrd-cli/tests/mcp.rs` (#209) |
| `wyrd-fakeagent` | the scripted test agent | `wyrd run` detects it (`--agent fakeagent`) | everything the Claude Code adapter reports, driven by a `.wyrdscript.toml` | — | as Claude Code | scenario tests (`make test-scenarios`, `make test-integration`, `make demo`) |

An agent that `wyrd run` does not recognise is recorded with adapter
`generic`: no hooks are injected, but a wrapped MCP server, OTLP spans,
and the kernel effects are still captured.

## What the coverage means for attribution

The correlator links each kernel effect to the tool call that caused it
([correlator](architecture/correlator.md)). With hooks, a command call's
exec is matched directly (confidence 1.0) or by its window (0.8). With
only the gateway or OTLP, calls still have windows, so attribution works
at window confidence. Without any intent path, every effect is
`unattributed` and divergence rule D004 fires for all of them: the
session is still a complete record of *what happened*, with no *why*.

## What no path captures

- The agent's conversation with its model provider (no TLS
  interception): assistant text comes from the hooks, never from the
  wire.
- Tool calls the agent makes without going through its hooks or a
  wrapped MCP server. They are visible as kernel effects (and as D004).
- Anything outside the session's cgroup.

See [limitations](limitations.md) for the rest.
