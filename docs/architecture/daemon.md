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
