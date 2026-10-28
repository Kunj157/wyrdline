# The Hub

The Hub is the team server for Wyrdline bundles. Members upload signed
`.wyrd` bundles (from `wyrd push`, or from a GitHub Actions job), and
they can browse, download, and search them, and see analytics across
the organisation's sessions. Studio can open the Hub's dashboard and
search pages.

The Hub verifies every upload before storing it: the signature, every
file hash, and the ledger's hash chain. A bundle that does not verify is
refused, and the answer names the failing check. The stored bundle is
the same file you exported, so anyone can still check it with
`wyrd verify`.

This page is for the people who run a Hub and for the people who upload
to it. The architecture and the full API are in
[the Hub's architecture page](architecture/hub.md).

Some features on this page are in open pull requests that are part of
1.0 and not yet merged. Each section names the feature's status. If a
section says a feature is not in your version, use the release notes to
check.

## What you need

- PostgreSQL, for accounts, organisations, tokens, and bundle records.
- An S3-compatible bucket (AWS S3, MinIO, and others). Bundles are stored
  there, not in PostgreSQL.
- A way to reach the Hub over HTTPS. The Hub itself serves plain HTTP on
  port 7730; put a TLS-terminating proxy or ingress in front of it.
- Optional: an OIDC provider for browser login (GitHub, Google, or any
  OpenID Connect issuer). Without one, only API tokens work.

## Run it

Two ways. Both come from open pull requests at the time of writing (the
image from #210, the chart from #213):

- **Container image** (`ghcr.io/kunj157/wyrdline-hub`, from
  `deploy/docker/hub.Dockerfile`). It is a distroless image that runs as
  a non-root user, with no shell. Pin it by digest. The image does not
  enable the analytics feature (see [analytics](#analytics-and-search)).
- **Helm chart** (`deploy/helm/wyrdline-hub`). It deploys the image with
  a Deployment, a Service, a PodDisruptionBudget, a NetworkPolicy, and
  optional ingress and autoscaling. It does not create PostgreSQL, the
  bucket, or any secret: you create the secrets and name them in the
  values.

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

_From #198 (upload, browse, download)._

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

_From #216 (CI upload tokens)._

A job can upload a bundle without storing a secret, by exchanging the
job's GitHub OIDC token for a short-lived upload token. This needs:

1. The job's `permissions: id-token: write`.
2. An organisation admin adds a trust rule for your repository
   (`POST /api/v1/orgs/{org}/ci-trust`, with `repository_owner` and
   `repository`). Omit `repository` to trust every repository of an owner.
3. The job exchanges its token at `POST /api/v1/ci/token` and uploads
   with the returned `<upload token>`. The token lasts 15 minutes by
   default and can only upload.

The script `integrations/github-action/scripts/hub-upload.sh` wraps those
steps. The Action's `hub-url` input is reserved and does not upload yet.

## Browse, download, search

- `GET /api/v1/orgs/{org}/bundles` lists bundles, newest first. Filter
  with `repo`, `rule` (bundles whose divergence rules include that rule),
  and `since`. Page with `before`.
- `GET /api/v1/bundles/{id}/download` downloads the stored `.wyrd`.
- Studio's Hub pages: the dashboard at `/hub`, and search at `/hub/search`.
  The Hub serves the Studio build named by `WYRD_HUB_STUDIO_DIR`; the
  container image does not include one.

Search takes a query of terms like `repo:owner/repo rule:D002 path:src/`.
The filters `rule:`, `repo:`, `agent:`, and `session:` match bundles;
`path:`, `kind:`, `host:`, `exe:`, and `tool:` match events. Event
filters need the analytics feature (below).

## Analytics and search

_From #220 (analytics, search, Studio pages)._

The analytics endpoints (`GET /api/v1/orgs/{org}/stats`, and event
search) and the Studio dashboard are in the analytics PR. They need a
Hub built with the `analytics` Cargo feature, which is not enabled in the
default container image. A Hub without it answers `501` for those
endpoints; bundle listing and bundle search still work.

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
