# wyrdd — the per-user daemon

`wyrdd` serves the Wyrdline HTTP API (plan §14.1) over the session data
directory. It runs unprivileged, as the user who owns the sessions.

## Listeners

| Listener | Default | Who may call |
|---|---|---|
| Unix socket | `$XDG_RUNTIME_DIR/wyrdline/wyrdd.sock` (0600, directory 0700) | Only peers with the daemon's UID (`SO_PEERCRED`); others are dropped before a byte is read |
| Loopback TCP | `127.0.0.1:7717` (`--listen`, loopback only; `--no-tcp` to disable) | Requests with `Authorization: Bearer <token>` |

The token lives in `$XDG_STATE_HOME/wyrdline/token` (0600, directory 0700),
is created on first start, and is compared in constant time. Browsers get
CORS only for the daemon's own origin (Studio is served by `wyrdd`) and an
optional development origin from `WYRD_STUDIO_ORIGIN`. No cookies are used,
so cross-site requests cannot ride on ambient credentials.

A stale socket file is replaced on start; a socket a live daemon still
answers on is an error. SIGINT/SIGTERM remove the socket.

## Conventions

- Every response carries `X-Request-ID`: the client's value if it is at
  most 128 visible ASCII characters, else a new ULID.
- Errors are RFC 9457 `application/problem+json` with a stable `type`
  (`unauthorized`, `not-found`, `not-ready`, `internal`, …).
- Logs are JSON on stderr (`WYRD_LOG`, default `info`), one line per
  request with `request_id`, method, path, status, and latency. The token
  is never logged.

## Endpoints so far

| Endpoint | Purpose |
|---|---|
| `GET /v1/health` | Liveness and version |
| `GET /v1/ready` | 200 when the data directory is writable, else 503 |
| `GET /v1/capabilities` | `wyrd doctor` report (Landlock probed through `wyrd __probe-landlock`) |
| `GET /v1/metrics` | Prometheus text: requests by status class |
| `GET /v1/sessions?status=&agent=&repo=&since=` | Sessions, newest first; `repo` is a workspace path prefix, `since` ms since the epoch |
| `GET /v1/sessions/{sid}` | Session record plus `head_seq`, `head_hash`, `sealed` |
| `GET /v1/sessions/{sid}/events?plane=&kind=&after_seq=&limit=` | A page of events (default 500, max 5 000); `kind` is exact or a dotted prefix; `next_after_seq` is null on the last page |
| `GET /v1/sessions/{sid}/graph?from_seq=&to_seq=` | Correlator graph; attribution is computed over the whole session, then calls and effects are cut to the range |
| `GET /v1/sessions/{sid}/tool-calls/{tcid}` | One call and every effect attributed to it or listing it as a candidate |
| `GET /v1/sessions/{sid}/verify` | Chain and seal verification (`ok`, first problem) |
| `GET /v1/sessions/{sid}/checkpoints` | Checkpoints, branches, current branch |

## Session and checkpoint ids

Session ids are lowercase ULIDs; a unique prefix is accepted, as in the
CLI. Ids are checked before any lookup: malformed → 400 `invalid-id`,
unknown → 404, ambiguous prefix → 409 `ambiguous`.

Checkpoint ids are per session (`c1`, `c2`, …), so checkpoint routes are
scoped under their session (`/v1/sessions/{sid}/checkpoints/…`) rather
than the global `/v1/checkpoints/{cid}` sketched in plan §14.1.

## Live sessions

`wyrd run` owns a live session: it holds the agent, the session's single
ledger writer, and the intent socket. The daemon reads the ledger
read-only (SQLite WAL readers never block the writer) and reaches a live
session only through that intent socket.

## Live stream

`GET /v1/sessions/{sid}/stream` sends each ledger event as a Server-Sent
Event (`id: <seq>`, `event: ledger`, `data: <event JSON>`). Clients resume
with `Last-Event-ID: <seq>` (or `?after_seq=`) and receive seq + 1 onward,
exactly once. The daemon polls the ledger read-only every 250 ms and sends
at most 500 events per poll, so a slow client holds the stream back rather
than growing a buffer. A keep-alive comment goes out every 15 s. When the
session is closed or failed and everything is sent, an `end` event carries
the final seq and the stream finishes.

## Session operations

`target` is a checkpoint id (`c3`), a branch name, or `head`; names are
limited to letters, digits, `-` and `_`.

| Endpoint | Body | Answer |
|---|---|---|
| `POST /v1/sessions/{sid}/stop` | — | `{ stopped }` (false if no agent was running) |
| `POST /v1/sessions/{sid}/checkpoints` | — | 201, the checkpoint |
| `POST /v1/sessions/{sid}/checkpoints/{target}/rewind` | `{ relaunch? }` | 200 `{ branch }`, or 202 `{ pid }` with relaunch |
| `POST /v1/sessions/{sid}/checkpoints/{target}/fork` | `{ name, relaunch? }` | 201 `{ branch }`, or 202 `{ pid }`; existing name → 400 |
| `GET /v1/sessions/{sid}/diff/{a}/{b}` | — | `{ changes }` |
| `POST /v1/sessions/{sid}/checkpoints/{target}/apply` | `{ dry_run?, force? }` | the apply report; real workspace changed since the session started → 409 `workspace-changed` listing the paths, unless `force` |

Each operation writes the same ledger event as the CLI. A live agent is
owned by `wyrd run`, so with `relaunch: true` the daemon starts a detached
`wyrd rewind|fork … --relaunch` (output in `relaunch.log` in the session
directory) instead of rewinding itself. Applying only a subset of paths
(`paths` in plan §14.1) is not supported yet.

An empty body reads as `{}`; malformed bodies and unknown fields answer
422 `invalid-body`.

## Idempotency

POST and PATCH accept `Idempotency-Key` (1–128 visible ASCII). The first
response for a key is kept for 24 h (at most 1 000 keys, oldest evicted)
and replayed with `Idempotent-Replayed: true`. The same key with a
different method, path, or body → 422 `idempotency-key-reused`; while the
first request still runs → 409 `idempotency-in-flight`. 5xx answers are
not kept, so a retry can succeed. Keys live in memory: a daemon restart
forgets them.

## Ingest: hooks and OTLP

| Endpoint | Body |
|---|---|
| `POST /v1/sessions/{sid}/otlp/v1/traces` | OTLP/HTTP **JSON** trace export; answers `{ "partialSuccess": {} }` |
| `POST /v1/sessions/{sid}/ingest/hooks/{adapter}` | A hook payload; answers the session's response (a decision for pre-execution hooks) |

Both forward to the live session's intent socket: `wyrd run` stays the
session's only ledger writer, and spans are redacted at ingest like every
other intent. A session that is not recording answers 409
`session-not-live`; protobuf bodies answer 415 (`OTEL_EXPORTER_OTLP_PROTOCOL=http/json`
is required, which keeps protobuf out of the dependency tree). Bodies may be
up to 8 MiB.

On TCP these two routes take a **per-session ingest token**, never the
daemon token: `BLAKE3-keyed(key = BLAKE3(daemon token), "wyrdline ingest v1\0" ‖ sid)`.
It opens only that session's ingest routes, so an agent can hold it
without reaching the mutating API (plan §17.8). `wyrd run` gives it to the
agent through `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `…_PROTOCOL=http/json`
and `…_HEADERS`, unless the user already set an OTLP endpoint
(`WYRD_DAEMON_LISTEN` overrides the address).

## Contract

`docs/api/openapi.json` (OpenAPI 3.1) and the TypeScript types in
`packages/schemas/src/generated-api/` are generated from the handler types
in `wyrdd::api` by `make schemas` (`wyrdd-gen-api`), using the same
schemars/ts-rs derives as the event schema — the plan named utoipa, but it
cannot reuse schemars schemas, so one derive system describes each type.
`packages/schemas/src/client.ts` is a typed client over those types.

Checks: `make schemas-check` (CI) fails on stale generated files; a Rust
test compares the routes registered in `routes/mod.rs` with the document
and validates real responses (successes and problems) against their
schemas; a Vitest test checks that every client call hits a documented
route and method.
