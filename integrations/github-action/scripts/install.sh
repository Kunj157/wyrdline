#!/usr/bin/env bash
# Install wyrd and wyrd-sensord for the Wyrdline action.
#
# Either from prebuilt binaries (WYRD_PATH: a directory holding `wyrd` and
# `wyrd-sensord`), or from a GitHub release of WYRD_REPOSITORY:
#   wyrdline-<arch>-unknown-linux-gnu.tar.gz   the binaries
#   SHA256SUMS                                 sha256sum lines for the assets
#   SHA256SUMS.sigstore.json                   cosign keyless bundle over SHA256SUMS
# The checksum is always verified. The signature must verify: the default
# (WYRD_VERIFY_SIGNATURE=required) refuses a release without a valid
# signature. =auto verifies when present and warns otherwise; =skip (or
# =false) installs without a signature check, with a warning in the log and
# the job summary. Prebuilt binaries (WYRD_PATH) are not downloads and skip
# this check.
#
# Env: WYRD_VERSION (tag or "latest"), WYRD_REPOSITORY, WYRD_PATH,
#      WYRD_VERIFY_SIGNATURE, WYRD_INSTALL_DIR, WYRD_RELEASE_BASE (tests),
#      WYRD_SIGNER_IDENTITY (regexp of the signing workflow identity).
set -euo pipefail

die() {
  echo "::error::wyrdline install: $*" >&2
  exit 1
}

repo=${WYRD_REPOSITORY:-Kunj157/wyrdline}
version=${WYRD_VERSION:-latest}
verify=${WYRD_VERIFY_SIGNATURE:-required}
dest=${WYRD_INSTALL_DIR:-${RUNNER_TEMP:-/tmp}/wyrdline/bin}
identity=${WYRD_SIGNER_IDENTITY:-^https://github\.com/${repo}/\.github/workflows/release\.yml@refs/tags/v}

case "$verify" in auto | required | skip | false) ;; *) die "verify-signature must be required, auto, or skip (got '$verify')" ;; esac
if [ "$verify" = false ]; then verify=skip; fi
mkdir -p "$dest"

# Warnings also go to the job summary, so an unverified install is visible
# on the run page and not only in the log.
note() {
  echo "::warning::wyrdline install: $*"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then printf '> **Warning:** %s\n\n' "$*" >>"$GITHUB_STEP_SUMMARY"; fi
}

publish() {
  "$dest/wyrd" --version >/dev/null || die "the installed wyrd does not run"
  if [ -n "${GITHUB_PATH:-}" ]; then echo "$dest" >>"$GITHUB_PATH"; fi
  if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "bin-dir=$dest" >>"$GITHUB_OUTPUT"; fi
  echo "wyrdline: $("$dest/wyrd" --version) in $dest"
}

if [ -n "${WYRD_PATH:-}" ]; then
  for b in wyrd wyrd-sensord; do
    [ -x "$WYRD_PATH/$b" ] || die "wyrd-path: no executable $b in $WYRD_PATH"
    install -m 0755 "$WYRD_PATH/$b" "$dest/$b"
  done
  echo "wyrdline: using prebuilt binaries from $WYRD_PATH (no checksum: they were not downloaded)"
  publish
  exit 0
fi

case "$(uname -m)" in
  x86_64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) die "unsupported architecture $(uname -m)" ;;
esac
asset="wyrdline-${arch}-unknown-linux-gnu.tar.gz"

if [ -n "${WYRD_RELEASE_BASE:-}" ]; then
  base=$WYRD_RELEASE_BASE
elif [ "$version" = latest ]; then
  base="https://github.com/${repo}/releases/latest/download"
else
  base="https://github.com/${repo}/releases/download/${version}"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fetch() { curl -fsSL --retry 3 -o "$work/$1" "$base/$1"; }

fetch "$asset" || die "cannot download $base/$asset (is $version a release with Linux binaries?)"
fetch SHA256SUMS || die "the release has no SHA256SUMS; refusing an unverified install"

signed=false
if fetch SHA256SUMS.sigstore.json; then signed=true; fi
case "$verify:$signed" in
  skip:*) note "SIGNATURE VERIFICATION DISABLED (verify-signature: false/skip). The release is installed without a signature check; only its checksum, which comes from the same host, was verified. Use the default (required) unless you accept that." ;;
  required:false) die "the release has no valid SHA256SUMS.sigstore.json and verify-signature is required (the default). Use a signed release, or set verify-signature: skip to accept an unsigned one." ;;
  auto:false) note "the release is not signed; only its checksum was verified (verify-signature: auto)" ;;
  *:true)
    command -v cosign >/dev/null || die "cosign is needed to verify the release signature"
    cosign verify-blob \
      --bundle "$work/SHA256SUMS.sigstore.json" \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \
      --certificate-identity-regexp "$identity" \
      "$work/SHA256SUMS" >/dev/null || die "the release signature does not verify"
    echo "wyrdline: SHA256SUMS signature verified ($identity)"
    ;;
esac

line=$(awk -v f="$asset" '$2 == f || $2 == "*" f' "$work/SHA256SUMS")
[ -n "$line" ] || die "SHA256SUMS has no entry for $asset"
(cd "$work" && printf '%s\n' "$line" | sha256sum --check --status) || die "checksum mismatch for $asset"
echo "wyrdline: $asset checksum verified"

mkdir -p "$work/x"
tar -xzf "$work/$asset" -C "$work/x" --no-same-owner
for b in wyrd wyrd-sensord; do
  # Only the binaries in bin/: the tarball also holds deploy/apparmor/wyrd, which
  # has the same name and was picked up on arm64 (find order is not fixed).
  found=$(find "$work/x" -type f -path "*/bin/$b" | head -n 1)
  [ -n "$found" ] || die "$asset has no $b"
  install -m 0755 "$found" "$dest/$b"
done
publish
