# Agent adapters

What each supported agent reports to Wyrdline, how it is wired up, and
how far it has been verified (plan §15.2). Effects (Phase 5) are captured
from the kernel for every agent regardless of its adapter.

| Agent | Adapter (`wyrd hook <name>`) | Wiring by `wyrd run` | Intent captured | Verified |
|---|---|---|---|---|
| Claude Code | `claude-code` | automatic: `--settings` with all hooks; stdio MCP servers routed through the gateway with `--mcp-config … --strict-mcp-config` | prompts, every tool call (pre/post, exit code, duration), denials, assistant messages (transcript + final), MCP traffic | recorded payloads of Claude Code 2.1 are the test fixtures; real-agent runs in a Cell |
| Codex CLI | `codex` | manual (see below) | same as Claude Code: Codex hooks use the same payload and decision format (`hook_event_name`, `tool_use_id`, `tool_input`, `tool_response`, `last_assistant_message`, `hookSpecificOutput.permissionDecision`) | field names checked against Codex 0.160; no live hook run yet |
| Gemini CLI | `gemini-cli` | manual (see below) | prompts (`BeforeAgent`), tool calls (`BeforeTool`/`AfterTool`, paired by a hash of tool + input), final message (`AfterAgent`); no exit codes or durations | built from the documented contract; not installed on the development machine, not verified live |
| Any MCP client | — (gateway) | wrap each server as `wyrd mcp-proxy --server <name> -- <cmd>` | MCP requests/responses, `tools/call` as tool calls, decisions, tool pinning | gateway e2e tests |
| OpenTelemetry GenAI | — (OTLP) | receiver lands with the daemon API (Phase 7) | `execute_tool` spans as tool calls; model-call spans counted, not recorded (no event kind in schema v1) | mapping unit tests |
| `wyrd-fakeagent` | speaks `claude-code` | automatic (sets `--agent fakeagent`) | everything Claude Code reports | scenario tests |

Without hooks (manual wiring skipped), an agent's session still records
its MCP traffic (if routed through the gateway) and — from Phase 5 — its
kernel effects; the correlator then attributes effects with reduced
confidence (plan §17.1, window matching).

## Why Codex and Gemini are not wired automatically

Claude Code accepts an extra settings file on its command line, so hooks
can be added without touching any user file. Codex reads hooks from its
own configuration (`hooks.json` under its home or the project), and
Gemini CLI from its settings files; injecting there would modify user or
project files — exactly what `wyrd run` promises not to do. `wyrd run`
detects both by program name, records the adapter, and prints how to
register the hook.

## Manual wiring

Inside a session `wyrd run` provides `WYRD_INTENT_SOCKET`, `WYRD_MODE`,
and `WYRD_BIN`; outside a session `wyrd hook` is a silent no-op, so a
permanently registered hook is harmless.

Codex — register the command for `PreToolUse`, `PostToolUse`,
`UserPromptSubmit`, and `Stop` in Codex's hook configuration:

```
wyrd hook codex
```

Gemini CLI — in `.gemini/settings.json` (project) or the user settings:

```json
{
  "hooks": {
    "BeforeTool": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "wyrd hook gemini-cli" }] }],
    "AfterTool":  [{ "matcher": "*", "hooks": [{ "type": "command", "command": "wyrd hook gemini-cli" }] }],
    "BeforeAgent": [{ "hooks": [{ "type": "command", "command": "wyrd hook gemini-cli" }] }],
    "AfterAgent":  [{ "hooks": [{ "type": "command", "command": "wyrd hook gemini-cli" }] }]
  }
}
```

## Decisions per adapter

| Adapter | Deny | Ask | Allow |
|---|---|---|---|
| `claude-code`, `codex` | `hookSpecificOutput.permissionDecision: deny` + reason | `…: ask` | no output (the agent's own permission rules apply) |
| `gemini-cli` | `{"decision": "deny", "reason": …}` | treated as deny | no output |
| gateway | JSON-RPC error `-32001`, `data.wyrdline_reason` | treated as deny | forwarded |
