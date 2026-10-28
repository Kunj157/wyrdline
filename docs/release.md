# Releases

Every `v*` tag runs `.github/workflows/release.yml`, which builds, signs,
attests, smoke-installs, and publishes the release. Nothing is built or
signed on a developer machine.

The packaging is done by the scripts in `scripts/release/`, not by cargo-dist:
the `.deb` needs the sensor's systemd unit and AppArmor profile, a postinst
that reloads AppArmor, and fixed archive metadata, and the scripts keep that
in one reviewable place. cargo-dist is therefore not used for release builds.

## Assets

| Asset | What it is |
|---|---|
| `wyrdline-x86_64-unknown-linux-gnu.tar.gz`, `wyrdline-aarch64-unknown-linux-gnu.tar.gz` | `wyrdline/bin/{wyrd,wyrdd,wyrd-sensord,wyrd-hub}`, the Studio under `wyrdline/share/wyrdline/studio`, the systemd unit and AppArmor profile under `wyrdline/deploy/` |
| `wyrdline_<version>_amd64.deb`, `wyrdline_<version>_arm64.deb` | `/usr/bin/{wyrd,wyrdd,wyrd-sensord}`, `/usr/lib/systemd/system/wyrd-sensord.service`, `/etc/apparmor.d/wyrd`, the Studio in `/usr/share/wyrdline/studio` |
| `wyrdline-<binary>-<target>.cdx.json` | CycloneDX 1.5 SBOM per shipped binary and target (`cargo-cyclonedx`, from `Cargo.lock`) |
| `SHA256SUMS` | SHA-256 of every asset above |
| `SHA256SUMS.sigstore.json` | cosign keyless signature bundle over `SHA256SUMS` |

Each asset also has a GitHub build-provenance attestation (SLSA provenance
through `actions/attest-build-provenance`).

The package installs the sensor unit but does not enable it; a privileged
daemon starts only when you ask: `sudo systemctl enable --now wyrd-sensord`.

## Verifying a download

```sh
tag=v1.0.0
base=https://github.com/Kunj157/wyrdline/releases/download/$tag
curl -fsSLO "$base/wyrdline-x86_64-unknown-linux-gnu.tar.gz"
curl -fsSLO "$base/SHA256SUMS"
curl -fsSLO "$base/SHA256SUMS.sigstore.json"

# 1. The checksum file was signed by this repository's release workflow on
#    that tag, with a certificate issued by GitHub Actions (cosign >= 2.5).
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity "https://github.com/Kunj157/wyrdline/.github/workflows/release.yml@refs/tags/$tag" \
  SHA256SUMS

# 2. The asset matches the signed checksum.
sha256sum --check --ignore-missing SHA256SUMS

# 3. Or ask GitHub for the asset's build provenance.
gh attestation verify wyrdline-x86_64-unknown-linux-gnu.tar.gz --repo Kunj157/wyrdline
```

The GitHub Action does steps 1 and 2 on every run
(`integrations/github-action/scripts/install.sh`).

## Reproducibility

Binaries are built with `--locked`, `SOURCE_DATE_EPOCH` set to the tagged
commit's time, and `--remap-path-prefix` for the checkout and the cargo
home, from the release profile in `Cargo.toml` (`strip = "symbols"`,
`codegen-units = 1`). The `reproducible` job builds `wyrd`, `wyrdd`, and
`wyrd-sensord` twice from a clean target directory and fails, listing the
differing files, if the two builds are not byte-identical. It also compares
them with the binaries the build job shipped and fails on a mismatch too.

The tarball and the `.deb` are built by `scripts/release/package.sh` with
fixed ownership, order, and timestamps, so repackaging identical binaries
gives identical archives (`tests/release/scripts.test.sh` checks this).

## Version

The workspace version in `Cargo.toml` is set to the tag (without the `v`)
in the release build by `scripts/release/stamp-version.sh`, so `wyrd
--version` reports the released version. The Debian version replaces `-`
with `~` (`1.0.0-rc.1` → `1.0.0~rc.1`) so pre-releases sort before the
release.

## Pipeline

```
version ─┬─ studio ─ build (x86_64, aarch64) ─┬─ sign ─ smoke (both archs) ─┐
         ├─ sbom ───────────────────────────────┘                             ├─ publish
         ├─ reproducible (after build) ───────────────────────────────────────┤
gate (pull-request.yml) ──────────────────────────────────────────────────────┤
kernel-matrix ────────────────────────────────────────────────────────────────┘
```

- `gate` and `kernel-matrix` are the PR workflows called on the tag.
- `smoke` runs `install.sh` against the just-built assets with the signature
  required, refuses a tampered `SHA256SUMS`, verifies the attestation,
  installs the `.deb`, starts `wyrd-sensord` from the packaged unit, and
  removes the package again.
- `publish` creates the GitHub release with notes generated from the merged
  pull requests; a tag with a pre-release suffix becomes a pre-release.
- `workflow_dispatch` runs everything except `publish` with version
  `0.0.0-dev.<run>`, to rehearse the pipeline without a tag.

All actions are pinned by commit SHA with the version in a comment.
