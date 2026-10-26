# Hub

The Hub (`crates/wyrd-hub`, binary `wyrd-hub`) is the self-hosted team
server: organisations share verified `.wyrd` bundles, search and chart
their sessions, and annotate them together (plan §4.4, §6.6, §14.4).

```
browser / wyrd CLI ──HTTPS──▶ wyrd-hub (Axum)
                                 ├── PostgreSQL 17   orgs, users, memberships,
                                 │                   bundles, audit_logs (sqlx)
                                 ├── S3 / MinIO      bundle objects
                                 └── OIDC provider   login (Dex locally, GitHub via Dex)
```

## Configuration (environment)

| Variable | Default | Meaning |
|---|---|---|
| `WYRD_HUB_LISTEN` | `127.0.0.1:7730` | listen address |
| `WYRD_HUB_PUBLIC_URL` | `http://<listen>` | URL users reach the Hub at (OIDC redirect, cookie `Secure` flag) |
| `WYRD_HUB_DATABASE_URL` | required | `postgres://…` |
| `WYRD_HUB_DB_MAX_CONNECTIONS` | `10` | pool size (1–1000) |
| `WYRD_HUB_S3_ENDPOINT` | required | S3 base URL, path-style (`http://127.0.0.1:7733`) |
| `WYRD_HUB_S3_BUCKET` | required | bucket, created at start if missing |
| `WYRD_HUB_S3_REGION` | `us-east-1` | SigV4 region |
| `WYRD_HUB_S3_ACCESS_KEY_ID`, `WYRD_HUB_S3_SECRET_ACCESS_KEY` | required | credentials |
| `WYRD_HUB_OIDC_ISSUER`, `WYRD_HUB_OIDC_CLIENT_ID`, `WYRD_HUB_OIDC_CLIENT_SECRET` | unset | OIDC login; unset disables browser login |
| `WYRD_HUB_DATA_DIR` | `wyrd-hub-data` | local scratch (upload spooling, analytics projections) |
| `WYRD_LOG` | `info` | log filter; logs are JSON lines on stderr |

Secrets are wrapped so they never appear in `Debug` output or logs.

## Start-up and health

The Hub listens immediately. A background task connects to Postgres and
runs the migrations, retrying with exponential backoff (0.5 s doubling to
30 s) and logging every failure with its reason; another creates the
bucket. Until both are done:

- `GET /healthz` — 200 whenever the process is up (liveness).
- `GET /readyz` — 200 `{"ready":true,…}` when the database answers and the
  bucket is reachable, else 503 with one entry per failed check.
- `GET /metrics` — Prometheus text: `wyrd_hub_http_requests_total`
  (method, route template, status class),
  `wyrd_hub_http_request_duration_seconds`, `wyrd_hub_db_pool_connections`,
  `wyrd_hub_ready`, plus feature counters added by later endpoints.

Every response carries `X-Request-ID` (a client value is kept when it is
short visible ASCII, otherwise a ULID), and every request is logged with
it, the method, the route template, the status, and the latency.

## Schema

Migrations live in `crates/wyrd-hub/migrations` as reversible pairs
(`<version>_<name>.up.sql` / `.down.sql`) and are listed in
`db::MIGRATOR`. Versions are timestamps (`YYYYMMDDNNNNNN`) so branches
developed in parallel do not collide. Integration tests apply every
migration, undo all of them, and apply them again.

| Table | Purpose |
|---|---|
| `orgs` | slug, name, quota (bytes, bundle count), retention days |
| `users` | OIDC `(issuer, subject)` identity, email and name for display |
| `memberships` | `(org, user) → viewer \| member \| admin` |
| `bundles` | verified manifest metadata, uploader-supplied repo, divergence rules, size, object key |
| `audit_logs` | actor, action, target, request id, details; append-only (a trigger rejects `UPDATE`, `DELETE`, `TRUNCATE`) |

## API (`/api/v1`)

Roles: `viewer` (read), `member` (+ upload, annotate), `admin` (+ delete,
members, retention). A resource in an organisation the caller does not
belong to answers **404**, exactly like one that does not exist.

| Method and path | Role | Issue |
|---|---|---|
| `GET /api/v1/auth/oidc/login` | — | #156 |
| `GET`/`POST /api/v1/auth/oidc/callback` | — | #156 |
| `POST /api/v1/auth/logout`, `GET /api/v1/me` | signed in | #156 |
| `POST`/`GET /api/v1/tokens`, `DELETE /api/v1/tokens/{id}` | signed in | #156 |
| `POST /api/v1/orgs`, `GET /api/v1/orgs/{org}` | signed in / viewer | #156 |
| `GET /api/v1/orgs/{org}/members`, `PUT`/`DELETE …/members/{user}` | viewer / admin | #156 |
| `GET /api/v1/audit-logs?org=` | admin | #156 |
| `POST /api/v1/orgs/{org}/bundles`, `GET …/bundles?repo=&rule=&since=` | member / viewer | #157 |
| `GET /api/v1/bundles/{bid}`, `GET …/download`, `DELETE …` | viewer / viewer / admin | #157 |
| `PATCH /api/v1/orgs/{org}` (quota, retention) | admin | #157 |
| `GET /api/v1/orgs/{org}/stats`, `GET …/search?q=` | viewer | #158 |
| `GET /api/v1/sync?bundle={bid}` (WebSocket) | viewer read-only, member+ write | #159 |

### Annotation sync endpoint (#159)

`GET /api/v1/sync?bundle={bid}` upgrades to a WebSocket carrying the
Automerge sync protocol for the annotation document of one bundle
(`crates/wyrd-sync` is the relay; `routes/sync.rs` mounts it):

- Authentication as for every other endpoint: the session cookie or
  `Authorization: Bearer <api token>`, checked before the upgrade. A
  cookie-authenticated upgrade must also carry the Hub's own `Origin`
  (browsers send it on every WebSocket handshake), so a page on another
  site cannot open a sync socket with the user's session: 403 otherwise.
- Authorisation before the upgrade: the bundle must exist in an
  organisation the caller belongs to (and that an org-bound token is bound
  to), else 404 like any resource in another organisation.
- Frames: binary frames are sync messages. The server sends one text
  frame first, `{"role":"viewer"|"member"|"admin"}`, so the client can
  disable editing for viewers. Viewers follow the document; a message
  from a viewer that carries changes is never applied: the server answers
  a last text frame `{"error":"read_only","message":…}` and closes with
  code 1008, and the client rolls back. The same shape with `too_large`
  (frame or document over the limit) or `invalid` (not a sync message).
  Text frames from clients close the socket with 1003; a client whose
  outgoing queue overflows is closed with 1013 and resyncs on reconnect.
- Persistence: one row per bundle in `bundle_annotations (bundle_id PK →
  bundles ON DELETE CASCADE, doc bytea, updated_at)` (migration
  `20261021000001_annotations`), written after every change that
  arrives and loaded when the first client of a bundle connects.
- Limits (`wyrd_sync::Limits::default`): 1 MiB per frame (enforced by the
  WebSocket layer and the relay), 8 MiB of changes per document, 64
  clients per bundle (503 beyond), 256 queued outgoing messages per
  client.
- Metrics: `wyrd_hub_sync_connections_total{role}`,
  `wyrd_hub_sync_messages_total{direction,outcome}` (`ok`, `read_only`,
  `too_large`, `invalid`), and the gauges `wyrd_hub_sync_clients`,
  `wyrd_hub_sync_documents`, `wyrd_hub_sync_document_bytes` (largest open
  document).

## Tests

`make test-hub` runs the Hub's tests, including the integration tests
that start Postgres 17 and MinIO with Testcontainers (Docker required; the
tests fail, they do not skip, without it). CI runs it in the `hub` job.
Upstream MinIO no longer publishes container images, so the tests use the
pinned community-maintained build `pgsty/minio`.
