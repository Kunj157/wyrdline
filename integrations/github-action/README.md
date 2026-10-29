# Wyrdline GitHub Action

Runs an AI coding agent in CI under Wyrdline: the agent works in a Cell
(copy-on-write workspace, cgroup, Landlock), `wyrd-sensord` records what its
processes really do in the kernel, and the session is sealed, exported as a
signed `.wyrd` bundle, uploaded as a workflow artifact, and summarised in
**one** PR comment (updated on every run) and in the job summary.

```yaml
permissions:
  contents: read
  pull-requests: write # the PR comment

jobs:
  agent:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@<sha> # pin by SHA
      - uses: Kunj157/wyrdline/integrations/github-action@<sha>
        with:
          command: claude -p 'fix the failing cart test'
          policy: .wyrdline/default.wasm
          fail-on: high
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

A complete workflow is in [`examples/`](examples/); this repository runs the
action on every change to it with the scripted `wyrd-fakeagent`, no API key
needed (`.github/workflows/github-action.yml`).

## What it does

1. **Install** `wyrd` and `wyrd-sensord` from a release of
   `wyrd-repository` (`wyrd-version`, default `latest`), or from prebuilt
   binaries in `wyrd-path`. The release's `SHA256SUMS` is always checked; its
   cosign keyless signature (`SHA256SUMS.sigstore.json`, issued to the
   repository's `release.yml` workflow on the release tag) must verify. This
   is the default (`verify-signature: required`): a release without a valid
   signature is refused. `auto` verifies when a signature is present and warns
   otherwise. `skip` (or `false`) installs without a signature check and
   writes a loud warning to the log and the job summary. Prebuilt binaries
   from `wyrd-path` are not downloaded and are not verified.
2. **Start the sensor**: installs `deploy/systemd/wyrd-sensord.service`
   (the runner has passwordless sudo) and waits for its socket. If it does
   not come up, the step fails with the `wyrd doctor` report and the agent
   never runs unprotected — unless `allow-degraded: true`.
3. **Run** `wyrd run -- <command>` in `workspace`, with `policy` if given.
   Ubuntu runners restrict unprivileged user namespaces, so the Cell's
   second stage goes through `sudo` and drops back to the runner user
   before the agent starts. Nobody answers policy *asks* in CI: they are
   denied after `ask-timeout` seconds.
4. **Seal and export**: `wyrd close`, `wyrd divergences --json`,
   `wyrd export --redact strict` (or `standard`), and with `apply: true`
   the agent's changes are written to the real workspace.
5. **Report**: the bundle becomes the artifact `wyrdline-<job>`; the job
   summary and the PR comment list every open divergence with its rule,
   severity, and the ledger sequence numbers of its evidence.
6. **Gate**: the step fails when the agent exits non-zero, when `fail-on`
   matches an open divergence, or when capture was incomplete (sensor lost,
   kernel rules not enforced) and `allow-degraded` is not `true`.

## Inputs

| Input | Default | |
| --- | --- | --- |
| `command` | — | Agent command. Shell quoting, no expansion: `$`, `` ` ``, `;`, `\|`, `&`, `<`, `>` are refused so nothing runs outside the Cell; use `bash -c '…'` for a script. |
| `agent` | detected | `claude-code`, `codex`, `gemini-cli`, `fakeagent`, `generic`. |
| `policy` | none | Wasm policy component, e.g. built with `wyrd policy build`. |
| `workspace` | `.` | Directory the agent works in. |
| `name` | | Session name. |
| `ask-timeout` | `5` | Seconds before an unanswered policy *ask* is denied. |
| `redact` | `strict` | Bundle redaction: `standard` or `strict`. |
| `apply` | `false` | Write the Cell's changes to the real workspace. |
| `fail-on` | | Rule ids (`D003`) and/or a severity threshold (`high` = high and critical). |
| `allow-degraded` | `false` | Run without (complete) kernel capture instead of failing. |
| `comment` | `true` | Post/update the PR comment. |
| `comment-key` | job id | One comment per key; set it when a job runs the action twice. |
| `github-token` | `github.token` | Used only for the comment; never passed to the agent. |
| `artifact-name` | `wyrdline-<job>` | Bundle artifact name (set it in matrix jobs). |
| `artifact-retention-days` | `14` | |
| `wyrd-version` | `latest` | Release tag to install. |
| `wyrd-repository` | `Kunj157/wyrdline` | |
| `wyrd-path` | | Prebuilt `wyrd` + `wyrd-sensord` directory (skips the download). |
| `verify-signature` | `required` | `required` (default: refuse unsigned releases), `auto` (verify when signed, warn otherwise), or `skip` / `false` (no signature check, warned). |
| `hub-url` | | Hub to upload the bundle to with the job's GitHub OIDC token (`id-token: write`, no stored secret). Empty keeps the bundle as a workflow artifact only. Needs `hub-org` and a trust rule on the Hub. |
| `hub-org` | | Organisation on the Hub to upload to (its slug); required with `hub-url`. |
| `hub-upload-required` | `true` | `true`: a failed upload fails the step, with the Hub's reason. `false`: it becomes a warning and the job continues. |

## Outputs

`session-id`, `bundle-path`, `divergence-count` (open), `agent-exit-code`,
`artifact-url`, `sensor` (`running` or `degraded`), `hub-bundle-id` and
`hub-bundle-url` (set when the bundle was uploaded to the Hub; empty
otherwise).

## Uploading a bundle to a Hub

Set `hub-url` and `hub-org` and the action uploads the bundle after the agent
has run, whatever its exit code, so a failed run's evidence reaches the Hub
too. It needs a job with `permissions: id-token: write`, and an organisation
admin on the Hub must have added a trust rule for the repository
(`docs/hub.md`, "From GitHub Actions"). The upload does not run without
`hub-url` and a bundle.

```yaml
permissions:
  contents: read
  id-token: write          # the upload's OIDC token
  pull-requests: write     # the PR comment
steps:
  - uses: Kunj157/wyrdline/integrations/github-action@<sha>   # pin a release
    with:
      command: claude -p 'fix the failing test'
      hub-url: https://hub.example.com
      hub-org: acme
    # Outputs: hub-bundle-id, hub-bundle-url
```

**Failure is loud by default.** A missing `id-token: write` permission, a
Hub that refuses the trust rule, an unreachable Hub, or a refused bundle
fails the step with the reason from `scripts/hub-upload.sh` (the same message
the script gives on its own). Nothing falls back silently. Set
`hub-upload-required: false` for jobs where the Hub is optional: the same
failure is then a warning, and the job continues. The bundle is still the
workflow artifact either way.

The agent step never gets a token. The upload step gets only the Hub URL,
the organisation, and the bundle path; the GitHub OIDC token is requested
from the runner by the upload script and exchanged for an upload token that
can only upload to `hub-org`.

The outputs are set for both a new upload and a bundle the organisation
already has. `hub-bundle-url` is the bundle's API resource
(`<hub>/api/v1/bundles/<id>`, the JSON record; `/download` on the same path
fetches the file). The Hub has no page per bundle.

The same script can run from your own step, with `WYRD_HUB_URL`,
`WYRD_HUB_ORG` and `WYRD_BUNDLE` set (`scripts/hub-upload.sh`).

## Release asset contract

The installer expects, per release: `wyrdline-<arch>-unknown-linux-gnu.tar.gz`
(`x86_64` or `aarch64`, containing `wyrd` and `wyrd-sensord` at any depth),
`SHA256SUMS`, and, once releases are signed, `SHA256SUMS.sigstore.json`
(`cosign sign-blob --bundle`). The release pipeline must publish these names.

## Security

- The PR's code and the agent are untrusted. The action passes no secret to
  the agent: the token input is only read by the reporting step. Whatever
  the workflow itself puts in the environment (an API key) is the
  workflow's choice; keep such jobs off `pull_request_target`.
- Session text in the comment (paths, agent quotes) is escaped: no HTML,
  no table breaks, no @-mentions.
- Fork PRs get a read-only token: the comment is skipped with a warning and
  the summary is in the job summary.
- The sensor runs with the systemd unit's capability set; `allow-degraded`
  is off by default so a missing sensor is never silent.

## Requirements

GitHub-hosted `ubuntu-24.04` (or a Linux runner with systemd, passwordless
sudo, cgroup v2, BTF, `node` ≥ 20, `jq`, `curl`).

## Development

```sh
node --test integrations/github-action/tests/*.test.mjs
bash integrations/github-action/tests/install.test.sh
bash integrations/github-action/tests/sensord.test.sh
shellcheck integrations/github-action/scripts/*.sh integrations/github-action/tests/*.sh
```
