# Wyrdline as an MCP server (`wyrd mcp`)

An agent can inspect its own session and ask to be rewound, but it can
never rewind, apply, or change policy by itself (plan §14.3, §17.8).

## Tools

| Tool | Kind | Answers |
|---|---|---|
| `list_checkpoints()` | read-only | branches and checkpoints (id, branch, seq, files) |
| `show_changes_since(checkpoint_id)` | read-only | files added, removed, or modified from that checkpoint to the head of the current branch |
| `explain_effect(seq)` | read-only | one kernel effect: its event, process chain, and attributed tool call |
| `list_divergences()` | read-only | divergences with evidence seqs and triage state |
| `request_rewind(checkpoint_id, reason)` | records a request | `pending human confirmation`; nothing is rewound |

Every tool carries MCP annotations (`readOnlyHint` true for the first
four). An unknown checkpoint or seq, or a session that is not running, is
a tool error (`isError: true`); malformed calls are JSON-RPC errors
(`-32602`), unknown methods `-32601`. Lines longer than 1 MiB and broken
JSON are answered with an error and serving continues. The server speaks
protocol revisions 2024-11-05, 2025-03-26, and 2025-06-18 over stdio.

## Binding

The server serves exactly one session:

- **In a Cell** (`wyrd run` sets `WYRD_SESSION_ID` and
  `WYRD_INTENT_SOCKET`), every tool is answered by `wyrd run` over the
  intent socket (`{"type":"session","query":…}`), because the agent
  cannot read its ledger, checkpoints, or content store. `--session` may
  only name that same session.
- **Outside a Cell**, `--session <id>` (or `WYRD_SESSION_ID`) picks the
  session, read from the data directory.
- No session at all is a start-up error.

Register it with an agent like any stdio server, e.g. in `.mcp.json`:
`{"mcpServers": {"wyrdline": {"command": "wyrd", "args": ["mcp"]}}}`.

## Rewind requests

`request_rewind` appends `meta.rewind.requested` (checkpoint, reason
redacted at the `standard` level, source `mcp`); the request id is that
event's seq. At most 16 requests may wait at a time, and reasons are
limited to 2000 characters, so an agent cannot flood the ledger.

A human decides, once per request:

| Surface | Confirm | Reject |
|---|---|---|
| CLI | `wyrd rewind <session> --confirm <id> [--relaunch]` | `wyrd rewind <session> --reject <id> [--note …]` |
| Studio | Review → Confirm rewind (session screen, above the checkpoint tree) | Reject |
| API | `POST /v1/sessions/{sid}/rewind-requests/{id}` `{"confirm": true}` | `{"confirm": false, "note": …}` |

`wyrd rewind <session> --requests [--json]` and
`GET /v1/sessions/{sid}/rewind-requests` list requests with their state.
Confirming records `meta.rewind.confirmed` and then rewinds under the same
session lock (stopping the agent if it runs), so the decision always
precedes `state.checkpoint.rewound` in the ledger. Rejecting records
`meta.rewind.rejected`. Requests are projected from the ledger, so they
survive restarts.

## Why the agent cannot confirm

- The intent socket has no query that confirms, rejects, or rewinds
  (`SessionQuery` has exactly the five tool queries; a test pins it).
- `wyrd rewind --confirm` inside a Cell fails: the session record,
  ledger, and content store are read-denied by Landlock (privileged test
  `privileged_an_agent_cannot_confirm_its_own_rewind_request`).
- The daemon routes need the user's daemon token, which lives in the
  state directory the Cell cannot read; the session's control token does
  not open them (tested).
- In `--observe` mode there is no Cell and no rewind; the agent runs as
  the user, so these guarantees are a Cell-mode property.

## Conformance

Checked with the MCP inspector 2.10.1 in CLI mode against a recorded
Cell session. The inspector reads `--session` as its own option, so the
session is passed in the server's environment:

```
npx @modelcontextprotocol/inspector@2.10.1 --cli wyrd mcp \
  -e WYRD_SESSION_ID=<id> --method tools/list --strict
npx @modelcontextprotocol/inspector@2.10.1 --cli wyrd mcp \
  -e WYRD_SESSION_ID=<id> --method tools/call --tool-name request_rewind \
  --tool-arg checkpoint_id=c1 'reason=tests broke'
```

`initialize` negotiates 2025-06-18, `tools/list --strict` reports no
schema portability problems, every tool answers, unknown checkpoints and
seqs come back as tool errors (`isError`), and after `request_rewind`
the session is unchanged until a human decides. The same protocol
behaviour is tested in `crates/wyrd-cli/tests/mcp.rs` by driving the
binary over stdio.
