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
| `WYRD_HUB_CI_ISSUER` | `https://token.actions.githubusercontent.com` | OIDC issuer whose tokens `POST /api/v1/ci/token` accepts (GitHub Enterprise Server: `https://HOST/_services/token`) |
| `WYRD_HUB_CI_AUDIENCE` | the public URL | `aud` claim those tokens must carry (the action requests it) |
| `WYRD_HUB_CI_TOKEN_TTL_SECS` | `900` | lifetime of CI upload tokens, 60–3600 |
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
  `wyrd_hub_ready`, `wyrd_hub_logins_total` (outcome),
  `wyrd_hub_ci_token_exchanges_total` (outcome), plus feature counters
  added by later endpoints.

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
| `oidc_logins`, `sessions`, `api_tokens` | login state, browser sessions, bearer tokens (hashed; `scope` `full` or `upload`) |
| `ci_trust_rules` | per org: GitHub `repository_owner` and optional `repository` whose workflows may mint upload tokens |
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
| `POST /api/v1/ci/token` | a GitHub Actions OIDC token | #161 |
| `GET`/`POST /api/v1/orgs/{org}/ci-trust`, `DELETE …/ci-trust/{id}` | admin | #161 |
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

### CI upload tokens (#161)

A workflow uploads bundles without a stored secret. The GitHub Action
(`integrations/github-action/scripts/hub-upload.sh`, with
`permissions: id-token: write`) asks the runner for an OIDC token whose
`aud` is the Hub's public URL and posts it:

```
POST /api/v1/ci/token
{ "token": "<GitHub Actions OIDC JWT>", "org": "acme" }

200 { "token": "wyrdci_…", "token_type": "Bearer", "scope": "upload",
      "org": "acme", "expires_in": 900, "expires_at": "2026-10-21T12:15:00Z" }
```

The Hub verifies the JWT before touching the database: signature against
the issuer's JWKS (discovery at `WYRD_HUB_CI_ISSUER`, cached for an hour,
refreshed once on an unknown `kid` so key rotation works, at most every
30 s), `iss`, `aud` (= `WYRD_HUB_CI_AUDIENCE`), `exp`/`nbf`/`iat` with
60 s leeway, and that `repository` belongs to `repository_owner`. Then it
looks for a **trust rule** of the named organisation: an admin registers
`{ "repository_owner": "acme-gh", "repository": "acme-gh/app" }` (one
repository) or `{ "repository_owner": "acme-gh" }` (every repository of
that user or GitHub organisation) through `POST /api/v1/orgs/{org}/ci-trust`;
names match case-insensitively. Answers: a token that fails verification
→ **401** `ci-token-rejected` (reason only in the log and in
`wyrd_hub_ci_token_exchanges_total{outcome="failure"}`); no matching rule
or unknown organisation → **403** `ci-not-trusted`; issuer unreachable or
failing → **503** (the action retries).

The minted token is an `api_tokens` row with `scope = 'upload'`,
`expires_at` = now + `WYRD_HUB_CI_TOKEN_TTL_SECS`, owned by the
repository's **CI identity**: a user `(issuer = the CI issuer,
subject = repo:<owner/name>)` named `owner/name (GitHub Actions)`, which
holds a `member` membership of the organisation (so uploads are attributed
to it and the usual role checks apply). An upload-only token is accepted
by exactly two routes — `POST /api/v1/orgs/{its org}/bundles` and
`GET /api/v1/me` — and answers **403** `insufficient-scope` everywhere
else. Removing a trust rule deletes the memberships (and live upload
tokens) of CI identities no remaining rule covers. Every exchange writes
an audit row `ci.token.exchange` (repository, owner and repository ids,
actor, workflow ref, run id, ref, sha, event, expiry); rules are audited
as `ci.trust.add` / `ci.trust.remove`.

## Tests

`make test-hub` runs the Hub's tests, including the integration tests
that start Postgres 17 and MinIO with Testcontainers (Docker required; the
tests fail, they do not skip, without it). CI runs it in the `hub` job.
Upstream MinIO no longer publishes container images, so the tests use the
pinned community-maintained build `pgsty/minio`.
