# Hub restore test (plan §27)

Plan §27 asks for a restore test once before v1.0: restore the Hub into a
fresh kind cluster, re-verify all bundles, and record the time. This is that
test, run in CI. The procedure is the `Hub restore` job in
`.github/workflows/hub-chart.yml`, and the operator procedure is in
[docs/hub.md](../../hub.md#back-up-and-restore).

**Result: passed.** Run **42490850455** (`workflow_dispatch` on
`infra/hub-restore-test`, head `1fd67008`), job **128762676194**: success.

## What the job does

1. Builds the Hub image from the checkout and creates a kind cluster.
2. Installs PostgreSQL 17, MinIO and the chart (`ci/kind-values.yaml`), and
   waits for `/readyz`.
3. Seeds an organisation, an admin and an API token. The Hub issues tokens to
   signed-in users only, and CI has no OIDC provider, so the rows are written
   with the Hub's own token hash (SHA-256).
4. Creates three sessions with `wyrd run --observe`, exports each as a strict
   signed bundle, verifies it locally, and uploads it with `wyrd push`.
5. Backs up PostgreSQL with `pg_dump -Fc` and the bucket with `aws s3 sync`.
   It writes a checksum manifest of the backup.
6. Deletes the cluster, starts the clock, and creates a fresh kind cluster.
7. Restores the database (`pg_restore --clean --if-exists --exit-on-error`)
   and the bucket, using the same credentials, and reinstalls the chart.
8. Lists the organisation's bundles with the same API token, downloads each
   one, runs `wyrd verify` on it, and checks that its hash equals the hash of
   the uploaded file.

## Timings and counts

| Measure | Value |
|---|---|
| Restore time: fresh cluster to every bundle verified | **53 s** |
| Job, from start to finish (includes the image build and the first cluster) | 14:39:12 to 14:46:21 UTC (7 min 9 s) |
| Database dump (custom format) | 25 677 bytes |
| Bucket objects copied | 3 |
| Bundles uploaded | 3 |
| Bundles listed after restore | 3 |
| Bundles that fail `wyrd verify` after restore | 0 |
| Restored bundles whose hash differs from the uploaded file | 0 |

Uploaded bundles (SHA-256 of the file, and the session each came from):

| # | SHA-256 | Session |
|---|---|---|
| 1 | `3a41f799953426b61b5cc50ca5d4f21f76160c086065405a5c994981264fd87e` | `01m6xsx8bggf788cr2w50vgpb2` |
| 2 | `bb4bc14153f958872d415eb67510e746100806274598b9b23b38aafceef84c10` | `01m6xsx8kz2gwxwdzvf124ay4d` |
| 3 | `b94e78257fa9636ff6bf1178cd31361711647ac7eb69f412b7bc48b6080fbb12` | `01m6xsx8w2zkh2z1hfnr8g54qy` |

The restored API token worked on the new cluster without being re-created.

## Limits of this test

- The three sessions are synthetic: `wyrd run --observe` with a `sh -c` command
  and no agent, with degraded capture (`WYRD_ALLOW_DEGRADED=true`, no sensor).
  They exercise the Hub's storage and verification paths, not real agent data.
- The backup was taken with no uploads running. It is not a live backup under
  load, and it is not a point-in-time restore.
- Credentials were reused (the same database URL and S3 keys), as an operator
  would do. A restore into a cluster with new credentials was not tested.
- Bucket versioning and object lock (plan §27) were not exercised.
- Known cosmetic issue: the backup manifest lists its own file with the hash
  of an empty file, because it is written by the command that hashes the
  directory. It does not affect the restore.

## Runs before the passing one

- Run **42490024906** (`a6368dba`) failed in the bundle step with exit 127:
  the built `wyrd` was not on `PATH`. Everything before it passed, including
  the seeding. Fixed in the next commit, which adds `target/debug` to
  `GITHUB_PATH`.
- Run **42490850455** (`1fd67008`) is the passing run above.
