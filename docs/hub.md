# The Hub

The Hub is the team server for Wyrdline bundles. Members upload signed
`.wyrd` bundles (from `wyrd push`, or from a GitHub Actions job), and
they can browse and download them. Analytics and search across an
organisation's sessions are described below.

The Hub verifies every upload before storing it: the signature, every
file hash, and the ledger's hash chain. A bundle that does not verify is
refused, and the answer names the failing check. The stored bundle is
the same file you exported, so anyone can still check it with
`wyrd verify`.

This page is for the people who run a Hub and for the people who upload
to it. The architecture and the full API are in
[the Hub's architecture page](architecture/hub.md).

Everything on this page is in the release. Analytics and search are in
their own section, [analytics and search](#analytics-and-search).

## What you need

- PostgreSQL, for accounts, organisations, tokens, and bundle records.
- An S3-compatible bucket (AWS S3, MinIO, and others). Bundles are stored
  there, not in PostgreSQL.
- A way to reach the Hub over HTTPS. The Hub itself serves plain HTTP on
  port 7730; put a TLS-terminating proxy or ingress in front of it.
- Optional: an OIDC provider for browser login (GitHub, Google, or any
  OpenID Connect issuer). Without one, only API tokens work.

## Run it

Two ways:

- **Container image** (`ghcr.io/kunj157/wyrdline-hub`, from
  `deploy/docker/hub.Dockerfile`). It is a distroless image that runs as
  a non-root user, with no shell. Pin it by digest. The image does not
  enable the analytics feature (see [analytics](#analytics-and-search)).
- **Helm chart** (`deploy/helm/wyrdline-hub`). It deploys the image with
  a Deployment, a Service, a PodDisruptionBudget, a NetworkPolicy, and
  optional ingress and autoscaling. It does not create PostgreSQL, the
  bucket, or any secret: you create the secrets and name them in the
  values.
  The chart's default image is the release's own version
  (`ghcr.io/kunj157/wyrdline-hub:<version>`); install the packaged chart
  from the release (`wyrdline-hub-<version>.tgz`). A chart built from the
  repository has no release image and refuses to render until you set
  `image.tag` or `image.digest`. **In production set `image.digest`**: a
  digest pins the exact image, and a tag can be moved.

Required settings, both ways:

| Setting | Environment variable | Chart value |
|---|---|---|
| Public address users reach | `WYRD_HUB_PUBLIC_URL` | `publicUrl` |
| Database URL (secret) | `WYRD_HUB_DATABASE_URL` | `database.existingSecret` (key `database-url`) |
| S3 endpoint and bucket | `WYRD_HUB_S3_ENDPOINT`, `WYRD_HUB_S3_BUCKET` | `s3.endpoint`, `s3.bucket` |
| S3 credentials (secret) | `WYRD_HUB_S3_ACCESS_KEY_ID`, `WYRD_HUB_S3_SECRET_ACCESS_KEY` | `s3.existingSecret` (keys `access-key-id`, `secret-access-key`) |

Set `WYRD_HUB_PUBLIC_URL` explicitly. Its default is `http://<listen>`,
which inside a container is `http://0.0.0.0:7730` and breaks the OIDC
redirect and the `Secure` cookie flag.

Health: `GET /healthz` says the process is up; `GET /readyz` says the
database and bucket answer. Metrics are at `GET /metrics` (Prometheus).

### Settings you may need

| Variable | Default | Meaning |
|---|---|---|
| `WYRD_HUB_LISTEN` | `127.0.0.1:7730` | Listen address (the image sets `0.0.0.0:7730`). |
| `WYRD_HUB_OIDC_ISSUER` | unset | Turns on browser login. Also needs `WYRD_HUB_OIDC_CLIENT_ID` and `WYRD_HUB_OIDC_CLIENT_SECRET`. |
| `WYRD_HUB_MAX_UPLOAD_BYTES` | 256 MiB | Largest bundle accepted (413 above it). |
| `WYRD_HUB_QUOTA_BYTES` | 10 GiB | Storage quota copied into each new organisation. |
| `WYRD_HUB_QUOTA_BUNDLES` | 10 000 | Bundle-count quota copied into each new organisation. |
| `WYRD_HUB_RETENTION_INTERVAL_SECS` | 3600 | Seconds between retention sweeps; `0` turns sweeps off. |
| `WYRD_HUB_DB_MAX_CONNECTIONS` | 10 | Database pool size. |
| `WYRD_LOG` | `info` | Log filter. Logs are JSON lines on stderr. |

The full list is in [the Hub's architecture page](architecture/hub.md).
The chart does not expose the quota, upload-limit, retention, or CI
settings; the defaults apply.

### The chart's defaults you should review

- `image.tag` defaults to the chart's `appVersion`, which is a placeholder.
  Set `image.tag` to the release you want, or set `image.digest`.
- The NetworkPolicy allows ingress only from the namespace listed in
  `networkPolicy.ingressFromNamespaces` (default `ingress-nginx`), and
  egress on TCP 443, 5432, and 9000 to any destination. Narrow the egress
  to your database and bucket if you can.
- The data and scratch volumes are `emptyDir`. Uploads that are being
  spooled, and analytics projections, do not survive a pod restart.
  Projections are rebuilt from the stored bundles on demand.
- Two replicas by default; the autoscaler is off until you enable it.
- The chart sets no TLS by default. Put ingress TLS in front of the Hub.

## Accounts and tokens

- **Browser login** uses your OIDC provider. Sessions last 7 days.
- **API tokens** are for scripts and `wyrd push`. Create one while signed
  in (`POST /api/v1/tokens`). Its secret is shown once. Tokens expire
  after 90 days by default (1 to 365 days). A token cannot create other
  tokens.
- **Roles** are per organisation: `viewer` (read), `member` (upload),
  `admin` (manage members, retention, CI trust, and deletion). An
  organisation you do not belong to looks the same as one that does not
  exist.

## Upload a bundle

From a machine with `wyrd` and an API token:

```sh
export WYRD_HUB_URL=https://hub.example.com
export WYRD_HUB_TOKEN=<api token>      # keep it in the environment, not in arguments
wyrd push session.wyrd --org my-team --repo owner/repo
```

`wyrd push` verifies the bundle locally before it sends anything. The
exit codes: `0` uploaded (or already on the Hub), `3` the bundle did not
verify, `4` a quota or conflict refusal, `5` the Hub is unreachable or
failing, `1` anything else. `--json` prints the Hub's answer.

Uploading the same bundle twice does not store it twice: the Hub answers
that it is already there.

### From GitHub Actions (CI token)

A job can upload a bundle without storing a secret, by exchanging the
job's GitHub OIDC token for a short-lived upload token. This needs:

1. The job's `permissions: id-token: write`.
2. An organisation admin adds a trust rule (`POST /api/v1/orgs/{org}/ci-trust`).
   A rule names one repository by default: `{"repository_owner": "acme-gh",
   "repository": "acme-gh/app"}`. Trusting every repository of an owner
   needs `"wildcard": true`; that opt-in is audited as `ci.trust.wildcard`.
   A rule may also pin a git ref, such as `"git_ref": "refs/heads/main"`,
   so only runs of that ref can upload (exact refs only, no patterns).
   Owner-wide rules made before this version were disabled by its
   migration: an admin deletes such a rule and adds it again with
   `wildcard: true` to trust the owner again.
3. The job exchanges its token at `POST /api/v1/ci/token` and uploads
   with the returned `<upload token>`. The token lasts 15 minutes by
   default and can only upload.

The script `integrations/github-action/scripts/hub-upload.sh` wraps those
steps. It sets two outputs for both a new upload and a bundle the
organisation already has: `hub-bundle-id` (the bundle's id) and
`hub-bundle-url`, the bundle's API resource, `<hub>/api/v1/bundles/<id>`.
The Hub has no page per bundle, so the URL is an API address. The Action's
`hub-url` input is reserved and does not upload yet.

## Browse and download

- `GET /api/v1/orgs/{org}/bundles` lists bundles, newest first. Filter
  with `repo`, `rule` (bundles whose divergence rules include that rule),
  and `since`. Page with `before`.
- `GET /api/v1/bundles/{id}/download` downloads the stored `.wyrd`.

## Analytics and search

The statistics endpoint (`GET /api/v1/orgs/{org}/stats`) and the search
endpoint (`GET /api/v1/orgs/{org}/search?q=`) are part of the Hub. So are
the Studio pages: the dashboard at `/hub` and search at `/hub/search`,
served when the Hub is given a Studio build with `WYRD_HUB_STUDIO_DIR`.

The analytics need a Hub built with the `analytics` Cargo feature. The
default container image does not enable it. A Hub without it answers `501`
for the statistics and search endpoints; listing and downloading bundles
still work.

Search takes a query of terms such as `repo:owner/repo rule:D002 path:src/`.
The filters `rule:`, `repo:`, `agent:`, and `session:` match bundles;
`path:`, `kind:`, `host:`, `exe:`, and `tool:` match events, and need the
`analytics` feature.

The statistics cover the 500 most recent bundles of the organisation. The
capture-overhead figure is always `null` per session: overhead is a
benchmark result ([sensor performance](results/perf-sensor.md)), not
something a session records.

## Limits

- Quotas cannot be changed through the API. An operator edits the
  organisation's row in the database.
- Retention sweeps delete up to 500 expired bundles per interval.
- Annotations are synced over a WebSocket per bundle, with 1 MiB frames
  and 8 MiB per document. Viewers can read them but not change them.
- The Hub is not a multi-tenant SaaS and has no billing. Organisations and
  roles are the extent of tenancy.
- The chart cannot set the CI token issuer, so the default (GitHub.com) is
  the only one it supports. GitHub Enterprise Server is not covered by the
  chart.
