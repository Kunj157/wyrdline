# Intent capture

How Wyrdline records what an agent *says* it does: prompts, tool calls,
their results, and the assistant's messages (plan §15.2).

## The intent socket

```
wyrd run (outside the Cell)
 ├─ binds $XDG_RUNTIME_DIR/wyrdline/intent-<session>.sock  (0600, owner only;
 │     falls back to <session dir>/intent.sock)
 ├─ serves it: one thread per connection, one JSON line in, one JSON line out
 └─ starts the agent with
      WYRD_INTENT_SOCKET  socket path
      WYRD_MODE           cell | observe
      WYRD_BIN            path of the wyrd binary

agent (inside the Cell)
 └─ hook → `wyrd hook claude-code` → socket → ledger
```

The socket accepts ingest (hook payloads and pings), the
[`wyrd mcp`](mcp-server.md) session views (read-only queries and rewind
*requests*), and asks answered with the session's control token. It
cannot start, stop, rewind, or apply anything. The server checks the peer's UID
(`SO_PEERCRED`) on top of the 0600 mode. Neither the socket nor the
ledger is inside the Cell's Landlock write set, so the agent cannot tamper
with either; it can only *report*, and its reports are untrusted. Kernel
effects (Phase 5) are the ground truth intent is compared against.

Everything is redacted at the `standard` level before it is appended
([redaction](redaction.md)).

## Events

| Hook (Claude Code) | Ledger events |
|---|---|
| `UserPromptSubmit` | `intent.message` (role user; starts a new turn) |
| `PreToolUse` | `intent.tool_call.started`, `guard.decision`, and `intent.tool_call.denied` when denied |
| `PostToolUse` / `PostToolUseFailure` | `intent.tool_call.finished` (status, exit code from `Exit code N`, duration, short summary), then a checkpoint |
| `Stop` / `SubagentStop` | one `intent.message` (role assistant) per text block of the turn |
| `SessionStart`, `SessionEnd`, others | ignored |

`Stop` texts come from the agent's transcript, read by the hook command
*inside* the Cell (the agent names the path; Wyrdline outside the Cell
never opens agent-supplied paths). The transcript may lag behind, so
`last_assistant_message` is appended when it is not already the last text.

## Decisions

`PreToolUse` (and the gateway's `tools/call`) is answered with a decision:

1. the static deny list, `wyrd run --deny-tool Bash --deny-tool
   'mcp__github__*'`;
2. then the Wasm policy of `wyrd run --policy policies/default.wasm`
   (`on-tool-call`, see [ADR-007](../adr/007-wasm-policy-host.md)), which
   sees the redacted arguments, the adapter, the workspace, and how many
   calls were denied before;
3. `ask` waits for a human: `wyrd ask <session>` lists pending asks and
   answers them (interactively, or `--allow ID` / `--deny ID`). With no
   answer within `--ask-timeout` (30 s), the call is denied.

Every step writes a `guard.decision` (an ask writes two: the `ask` and
the final answer, with who answered and how long it took). `allow` prints
nothing, so the agent's own permission rules still apply — Wyrdline only
ever narrows what the agent may do; `deny` returns
`permissionDecision: deny` with a reason.

Asks are answered over the intent socket, which the agent can reach too,
so an answer must carry the session's control token: 32 random bytes in
`control.token` (0600) in the session directory, removed when the run
ends. The Cell cannot read the session directory.

Failure handling:

| Situation | Cell mode | Observe mode |
|---|---|---|
| socket unreachable / timed out (45 s) | deny pre-checks ("wyrdline unavailable") | allow |
| malformed payload | recorded as `guard.decision` (policy `wyrdline.ingest`), deny | recorded, allow |
| ledger append fails | deny pre-checks | allow |
| not under `wyrd run` (no socket variable) | no-op | no-op |

Hooks other than pre-checks never block the agent.

## Checkpoint per tool call

After `intent.tool_call.finished`, a checkpoint attributed to that call is
scheduled 250 ms later. If the agent starts another call first, the
pending checkpoint is taken *before* the next call is answered, so each
checkpoint holds the effects of exactly one call. When the agent exits,
the pending call (if any) is attributed to the exit checkpoint; without
one, the exit checkpoint is only kept when something changed.

## Injection

`wyrd run` detects the adapter from the program name (`claude` →
`claude-code`) or takes `--agent`. For Claude Code it writes
`claude-code-settings.json` into the session directory and inserts
`--settings <file>` after the program name. No user or project file is
modified; the ledger keeps the command as the user typed it.

## MCP gateway

`wyrd mcp-proxy --server <name> -- <command…>` relays a stdio MCP server
(newline-delimited JSON-RPC) between the agent and the real server, which
runs as its child. Every message is reported to the intent socket:

| Message | Ledger events |
|---|---|
| request with an id (either direction) | `intent.mcp.request` (method, redacted params) |
| response | `intent.mcp.response` (error flag, short redacted summary) |
| `tools/call` | additionally `intent.tool_call.started` (source `mcp`, tool `mcp__<server>__<tool>`, call id `mcp:<server>:<id>`) and `guard.decision` (stage `gateway`); its response adds `intent.tool_call.finished` and schedules a checkpoint |
| notifications | not recorded |

A denied `tools/call` (`--deny-tool mcp__github__*` — the same names the
agent's own hooks use) is never forwarded; the agent receives a JSON-RPC
error with code `-32001` and `data.wyrdline_reason`. In batches, denied
elements are answered and the rest is forwarded. `ask` is treated as deny
in the gateway until interactive decisions exist (Phase 9). When the
socket is unreachable, `tools/call` is denied in Cell mode and forwarded
in observe mode; other messages always pass. The proxy exits with the
server's exit code; the server's stderr passes through.

**Tool pinning.** The first `tools/list` result pins each tool's
description and input schema (BLAKE3). If a later listing differs, a
`guard.decision` with policy `wyrdline.mcp-pinning` and decision `ask` is
recorded — a server that swaps a benign description for a malicious one
after approval ("rug pull") is flagged (divergence rule D009 builds on it).

**Routing the agent's servers.** For Claude Code, `wyrd run` merges the
MCP servers the agent would start (user config < project `.mcp.json` <
local project entry), rewrites every stdio server to go through
`wyrd mcp-proxy`, writes the result to the session directory (mode 0600,
since server environments can hold credentials; removed after the run),
and passes it with `--mcp-config <file> --strict-mcp-config`.

**Remote servers.** A Streamable HTTP server (`"type": "http"`) becomes a
stdio entry running `wyrd mcp-proxy --server <name> --remote <file>`: the
agent speaks stdio to the gateway, the gateway speaks Streamable HTTP to
the server, and recording, decisions, and pinning work exactly as for
local servers. Its URL and headers (often credentials) go to
`mcp-remote-<name>.json` in the session directory (0600, removed after the
run like the MCP config) rather than into argv. Each message is POSTed with `Accept: application/json,
text/event-stream`; JSON and SSE answers are relayed, `Mcp-Session-Id` is
kept, `MCP-Protocol-Version` is sent after `initialize`, and the session is
DELETEd when the agent closes. HTTP failures answer each request with a
JSON-RPC error. Not covered: server-initiated messages on a standalone GET
stream, and the legacy HTTP+SSE transport (`"type": "sse"`), which is
passed through unproxied. The client is `ureq` (rustls).
