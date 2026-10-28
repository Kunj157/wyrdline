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
| `WYRD_HUB_MAX_UPLOAD_BYTES` | 256 MiB | largest bundle accepted |
| `WYRD_HUB_QUOTA_BYTES`, `WYRD_HUB_QUOTA_BUNDLES` | 10 GiB, 10 000 | quota of new organisations |
| `WYRD_HUB_RETENTION_INTERVAL_SECS` | `3600` | retention sweep interval (0 disables) |
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

## Authentication

**Browser login** is OpenID Connect, authorization code with PKCE:

1. `GET /api/v1/auth/oidc/login?return_to=/path` stores a pending login
   (hashed `state`, `nonce`, PKCE verifier; 10 minutes) bound to the
   browser by an HttpOnly `wyrd_login` cookie, and redirects to the
   provider.
2. The provider redirects to `GET /api/v1/auth/oidc/callback?code&state`
   (a client that handles the redirect itself may `POST` `{code, state}`
   as JSON). The state is consumed once; the browser binding, expiry, the
   code exchange (client secret + verifier), and the ID token (signature
   against the provider's JWKS: RS256, ES256 or EdDSA, refreshed on an
   unknown `kid` for key rotation; `iss`, `aud`/`azp`, `exp` with 60 s
   leeway, `nonce`) are checked. Any failure answers 401 "Login failed"
   without detail and is logged with its reason and counted in
   `wyrd_hub_logins_total{outcome}`; an unreachable provider is 503.
3. The user is found or created by `(issuer, subject)`, a **new** session
   id is issued (no session fixation), and `wyrd_session` is set:
   HttpOnly, SameSite=Lax, Path=/, 7 days; on HTTPS also `Secure` with the
   `__Host-` prefix. Only `return_to` paths on the Hub itself are honoured.

GitHub is the production identity provider through Dex's GitHub
connector (GitHub's own OAuth does not issue ID tokens).

**API tokens** (`Authorization: Bearer wyrdhub_…`) are created from a
signed-in session with `POST /api/v1/tokens {name, org?, expires_in_days?}`
(default 90, at most 365 days); the secret is shown once. A token acts as
its user, optionally only in one organisation; tokens cannot create
tokens. Removing a membership deletes the user's tokens for that org.

Session ids, tokens, and login state are stored only as SHA-256 hashes.
**CSRF:** a cookie-authenticated `POST`/`PUT`/`PATCH`/`DELETE` must carry
the Hub's own `Origin` (or `Sec-Fetch-Site: same-origin`), else 403;
bearer-token requests are not subject to this.

**Roles** per organisation: `viewer` < `member` < `admin`. Any signed-in
user may create an organisation and becomes its admin; admins add,
change, and remove members (by user id or email; the user must have
signed in once) and cannot remove or demote the last admin (409).

**Audit log:** every mutation writes an `audit_logs` row in the same
transaction (actor, action, target, request id, details). Actions so far:
`auth.login`, `auth.logout`, `org.create`, `org.member.set`,
`org.member.remove`, `token.create`, `token.revoke`, `org.update`,
`bundle.upload`, `bundle.delete`, `bundle.expire`. Admins read their
organisation's log with `GET /api/v1/audit-logs?org=<slug>&limit=&before=`
(newest first).

## Bundles

`POST /api/v1/orgs/{org}/bundles` (member) takes `multipart/form-data`
with a `bundle` file and an optional `repo` label, and:

1. streams the file to a spool file under `WYRD_HUB_DATA_DIR/uploads`,
   refusing it with 413 as soon as it passes `WYRD_HUB_MAX_UPLOAD_BYTES`
   (nothing is buffered in memory; the spool file is always removed);
2. verifies it with `wyrd-bundle`'s reader (archive structure and size
   limits, signature over the manifest, manifest, every file hash, the
   ledger hash chain). A failure is **422** `bundle-rejected` with
   `failed_check` and the full `checks` list;
3. locks the organisation, answers 200 with the existing bundle if the
   same manifest was uploaded before, and checks the quota: bundle count
   (**429** `quota-bundles`) and stored bytes (**413** `quota-bytes`);
4. stores the object at `bundles/<org id>/<bundle id>.wyrd`, then inserts
   the row (session id, name, agent, signer fingerprint, event count,
   divergence rules raised, size, repo) and the `bundle.upload` audit
   entry in one transaction. If storage is down the answer is 503 and no
   row exists; if the insert fails the object is deleted again.

`GET /api/v1/orgs/{org}/bundles?repo=&rule=&since=&before=&limit=` lists
newest first (`since` is an RFC 3339 time or date, `before` a bundle id
for paging); `GET /api/v1/bundles/{bid}` shows one;
`…/download` streams it back byte for byte; `DELETE` (admin) removes the
row with a `bundle.delete` audit entry, then the object.

**Retention:** admins set `PATCH /api/v1/orgs/{org} {"retention_days":
N}` (1–3650, or `null` for none; audited as `org.update`). Every
`WYRD_HUB_RETENTION_INTERVAL_SECS` the Hub deletes bundles older than
that, each audited as `bundle.expire` by `system`.

**Quotas:** every organisation starts with `WYRD_HUB_QUOTA_BYTES` and
`WYRD_HUB_QUOTA_BUNDLES`. There is no self-service endpoint (an org admin
would raise their own limit); the operator changes `orgs.quota_bytes` and
`orgs.quota_bundles` in the database.

**From the CLI:** `wyrd push file.wyrd --hub <url> --org <slug> [--repo
owner/name]` with an API token in `WYRD_HUB_TOKEN`. It verifies the file
locally first, then streams it. Exit codes: 0 uploaded (or already
there), 1 usage or refused, 3 verification failed, 4 over quota, 5 Hub
unreachable.

Metrics: `wyrd_hub_uploads_total{outcome}` (`accepted`, `duplicate`,
`rejected`, `too_large`, `over_quota`, `storage_error`, `error`),
`wyrd_hub_bundle_verify_seconds`, `wyrd_hub_retention_sweeps_total`.

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
| `PATCH /api/v1/orgs/{org}` (retention) | admin | #157 |
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
