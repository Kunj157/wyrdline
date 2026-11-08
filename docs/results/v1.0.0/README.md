# Release evidence for v1.0.0

Index of what backs the 1.0 claims. Each row names the committed file or the
CI run it comes from, and what it shows. Numbers are copied from those files,
not restated from memory. The per-step status of the plan §23 system test
is in [system-test.md](system-test.md), which is the record of that run and
is not repeated here.

## Committed reports

| Evidence | File | What it shows |
|---|---|---|
| Full system test (plan §23) | [system-test.md](system-test.md) | Per-step results, the `make system-test` run (30 checks), the real-agent run (§23.4), CI evidence for prepare, observability and the Action test repository |
| Performance at 1.0 (plan §18.2) | [../perf-v1.0.md](../perf-v1.0.md) | Event loss, checkpoint, rewind, connect latency, capture overhead, with both runs and the method. Checkpoint p95 is 158.7 ms against 150 ms: missed, tracked in #239 |
| Enforcement latency | [../enforcement.md](../enforcement.md) | Denied-connect latency, and the escape-suite attempts with their outcomes |
| Attribution | [../attribution.md](../attribution.md) | Correlator precision and recall on 300 generated sessions with known ground truth (`wyrd-correlator`, test `eval`) |
| Divergence and claim evals | [../evals-baseline.json](../evals-baseline.json) | Per-rule precision and recall on 91 labelled divergence cases and 155 labelled assistant turns. The datasets are synthetic, so these are an upper bound for unseen agents |
| Studio river performance | [../studio-perf.md](../studio-perf.md) | Canvas2D at 60 fps (p95 frame 16.8 ms) with 200 000 events in headless Chromium; WebGPU on a software rasteriser at 30 fps. See the gap below |
| Sensor and earlier performance | [../perf-sensor.md](../perf-sensor.md) | Design history of the sensor's overhead and throughput |
| Threat model and 1.0 security review | [../../threat-model/scope.md](../../threat-model/scope.md), [../../threat-model/review-2026.md](../../threat-model/review-2026.md) | Trust boundaries, threats and their mitigations, and the review findings that were fixed before 1.0 |

## CI runs

Run ids are GitHub Actions run numbers in `Kunj157/wyrdline`, checked
against the workflow run list on 2026-11-08.

| Gate | Workflow and run | Result |
|---|---|---|
| PR gate on `dev`, latest green push | `pull-request.yml`, run 42482174081 (commit `72cbbea`) | All ten jobs green: Rust, Cell privileged integration, Hub Postgres and MinIO, Studio, Studio end to end, cargo-deny, secrets and SAST, fuzz 60 s per parser, actionlint, release scripts |
| Kernel matrix, latest nightly | `kernel-matrix.yml`, run 42465527897 (2026-11-08, `cafe2a2`) | **Red.** Kernels 6.1, 6.12, 7.0 and 7.2 failed in the step "Install QEMU and vmtest", before any test ran. Kernels 5.15 and 6.6 and both runner kernels (Ubuntu 24.04 and 26.04) are green. The strict sensor throughput job failed on the checkpoint target (#239) |
| Kernel matrix, last nightly with every VM and runner kernel green | `kernel-matrix.yml`, run 42357223643 (2026-11-07, `cc6c0cf`) | Kernels 5.15, 6.1, 6.6, 6.12, 7.0 and 7.2 and both runner kernels green. The strict sensor job failed on the checkpoint target (p95 483 ms against 150 ms, #239) |
| Security nightly, latest green | `security.yml`, run 42472648100 (2026-11-08, `cafe2a2`) | Green. This workflow runs the long fuzz job only: 10 minutes per untrusted-input parser |
| Hub image | `hub-image.yml` | Runs on pull requests that touch `deploy/docker/**` and on `v*` tags. Trivy fails on fixable HIGH or CRITICAL findings, and the image gets an SBOM |

Gates that run on every pull request, from `pull-request.yml`:
- **Gitleaks** (secret scan of the working tree and history) and **Semgrep**
  (the default registry rules and `.semgrep/wyrd.yml`), in the job
  "Security · secrets, SAST".
- **cargo-deny** (licences, advisories, bans and sources), in the job
  "Supply chain · cargo-deny".
- **Escape suite**: the `privileged_escape_*` tests in `tests/security/escape`
  run in the Cell job through `make test-integration`, and locally through
  `make test-escape`.
- **AppArmor profile**: the Cell job loads `deploy/apparmor/wyrd` and starts an
  unprivileged Cell with it, after showing that it fails without it.
- **Redaction corpus**: `crates/wyrd-bundle/tests/redaction.rs` and the
  export tests in `crates/wyrd-bundle/tests/` run in `make test-rust`.

## Produced by the release

`release.yml` runs on every `v*` tag and publishes the release at
`https://github.com/Kunj157/wyrdline/releases/tag/v1.0.0` (live once the tag
is pushed). Its assets:

- **SBOMs**: one CycloneDX SBOM per shipped binary and target,
  `wyrdline-<binary>-<target>.cdx.json`, generated from `Cargo.lock`.
- **Checksums and signature**: `SHA256SUMS` over every asset, and
  `SHA256SUMS.sigstore.json`, a cosign keyless signature by this repository's
  release workflow on the tag.
- **Build provenance**: GitHub attestations for the tarballs, the `.deb`
  files, the SBOMs and `SHA256SUMS`. The Hub chart is covered by the signed
  `SHA256SUMS` only, not by an attestation.
- **Reproducibility**: the `reproducible` job builds `wyrd`, `wyrdd` and
  `wyrd-sensord` twice and compares them, and compares them with the shipped
  binaries.
- **Hub image**: `ghcr.io/kunj157/wyrdline-hub` is pushed by digest, signed
  with cosign keyless, and gets build-provenance and SBOM attestations
  (`hub-image.yml`).

How to verify these is in [../../release.md](../../release.md).

## Not in this evidence

- **Screenshots.** There is no `docs/screenshots` directory. The four
  committed Studio images are the site's, in `apps/site/public/shots/`
  (`attribution.jpg`, `checkpoints.jpg`, `inspect.jpg`, `river.jpg`).
- **60 fps on a real GPU.** Not measured. There is no GPU on the development
  machine or the CI runners. The WebGPU figure above is from a software
  rasteriser (see [../studio-perf.md](../studio-perf.md)).
- **Judge cost per session.** Not measured: it needs a live
  `ANTHROPIC_API_KEY` ([#212](https://github.com/Kunj157/wyrdline/issues/212)).
- **The real-agent demo video.** Moved to a follow-up after 1.0
  ([#298](https://github.com/Kunj157/wyrdline/issues/298)).
- **Checkpoint latency target.** Missed by 9 ms on the development machine
  and above 150 ms on the hosted runner ([#239](https://github.com/Kunj157/wyrdline/issues/239)).
- **AppArmor profile on a hosted runner.** The Cell job checks it
  (`pull-request.yml`, "The AppArmor profile lets an unprivileged Cell start"):
  a Cell fails without `deploy/apparmor/wyrd` and starts with it. The check
  does not cover other kernels or an installed `.deb` (see
  [system-test.md](system-test.md), §23.1).

## Reproduce

```sh
make system-test     # plan §23 local steps, with checks (sudo)
make test-perf       # performance measurements (release build, sudo)
make test-evals      # divergence and claim evals against the committed baseline
make test-escape     # escape suite (sudo)
```
