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
