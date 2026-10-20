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
`org.member.remove`, `token.create`, `token.revoke`. Admins read their
organisation's log with `GET /api/v1/audit-logs?org=<slug>&limit=&before=`
(newest first).

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

### Annotation sync endpoint (for #159)

`GET /api/v1/sync?bundle={bid}` upgrades to a WebSocket carrying the
Automerge sync protocol (binary frames) for the annotation document of
one bundle:

- Authentication as for every other endpoint: the session cookie (with
  the `Origin` check that guards cookie-authenticated requests) or
  `Authorization: Bearer <api token>`; checked before the upgrade.
- Authorisation before the upgrade: the bundle must exist in an org the
  caller belongs to, else 404. Viewers receive changes but every change
  they send is rejected (the server never applies it and closes with an
  error message); members and admins may write.
- The server persists one document per bundle, in a
  `bundle_annotations (bundle_id PK → bundles ON DELETE CASCADE, doc bytea,
  updated_at)` table added by that issue's own timestamped migration, and
  seeds it from the bundle's `annotations.automerge` when present.
- Limits: frame and document size caps; every connection and rejected
  write is counted in `/metrics`.

## Tests

`make test-hub` runs the Hub's tests, including the integration tests
that start Postgres 17 and MinIO with Testcontainers (Docker required; the
tests fail, they do not skip, without it). CI runs it in the `hub` job.
Upstream MinIO no longer publishes container images, so the tests use the
pinned community-maintained build `pgsty/minio`.
