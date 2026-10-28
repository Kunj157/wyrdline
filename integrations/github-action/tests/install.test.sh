#!/usr/bin/env bash
# Tests for scripts/install.sh against releases faked on disk (file://).
# Run: bash integrations/github-action/tests/install.test.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
install_sh="$here/../scripts/install.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0
fail=0

case "$(uname -m)" in aarch64 | arm64) arch=aarch64 ;; *) arch=x86_64 ;; esac
asset="wyrdline-${arch}-unknown-linux-gnu.tar.gz"

# A release directory: binaries in a subdirectory, as cargo-dist lays out.
make_release() {
  local dir=$1
  mkdir -p "$dir/pkg/wyrdline"
  printf '#!/bin/sh\necho "wyrd 0.0.0-test"\n' >"$dir/pkg/wyrdline/wyrd"
  printf '#!/bin/sh\nexit 0\n' >"$dir/pkg/wyrdline/wyrd-sensord"
  chmod +x "$dir/pkg/wyrdline/"*
  tar -czf "$dir/$asset" -C "$dir/pkg" wyrdline
  (cd "$dir" && sha256sum "$asset" >SHA256SUMS)
}

# A cosign stand-in that records its arguments and exits with $1.
fake_cosign() {
  mkdir -p "$tmp/bin"
  printf '#!/bin/sh\necho "$@" >"%s/cosign.args"\nexit %s\n' "$tmp" "$1" >"$tmp/bin/cosign"
  chmod +x "$tmp/bin/cosign"
}

# expect <0|1> <name> [VAR=value ...]: run install.sh with a fresh install dir.
expect() {
  local want=$1 name=$2
  shift 2
  local out="$tmp/out-$((pass + fail))"
  rm -rf "$tmp/dest"
  env -i PATH="$tmp/bin:$PATH" HOME="$HOME" WYRD_INSTALL_DIR="$tmp/dest" GITHUB_OUTPUT="$tmp/gh-output" \
    "$@" bash "$install_sh" >"$out" 2>&1
  local got=$?
  if { [ "$want" = 0 ] && [ "$got" = 0 ]; } || { [ "$want" = 1 ] && [ "$got" != 0 ]; }; then
    pass=$((pass + 1))
    echo "ok   $name"
  else
    fail=$((fail + 1))
    echo "FAIL $name (exit $got)"
    sed 's/^/     /' "$out"
  fi
  LAST_OUT=$out
}

contains() {
  if grep -q -- "$2" "$1"; then
    pass=$((pass + 1))
    echo "ok   ↳ $3"
  else
    fail=$((fail + 1))
    echo "FAIL ↳ $3: no '$2' in output"
    sed 's/^/     /' "$1"
  fi
}

rel="$tmp/release"
make_release "$rel"
base="WYRD_RELEASE_BASE=file://$rel"

# The default is required: an unsigned release is refused.
expect 1 "unsigned release, default: refused" "$base"
contains "$LAST_OUT" "verify-signature is required" "reason given (default is required)"

expect 0 "unsigned release, auto: checksum verified, installed" "$base" WYRD_VERIFY_SIGNATURE=auto
contains "$LAST_OUT" "checksum verified" "checksum reported"
contains "$LAST_OUT" "not signed" "missing signature warned about"
contains "$tmp/gh-output" "bin-dir=$tmp/dest" "bin-dir output"
if [ -x "$tmp/dest/wyrd-sensord" ]; then pass=$((pass + 1)); else fail=$((fail + 1)) && echo "FAIL sensord not installed"; fi

rm -f "$tmp/summary"
expect 0 "unsigned release, auto: warning in the job summary" "$base" WYRD_VERIFY_SIGNATURE=auto GITHUB_STEP_SUMMARY="$tmp/summary"
contains "$tmp/summary" "not signed" "summary notes the unsigned release"

rm -f "$tmp/summary"
expect 0 "unsigned release, skip: installed with a loud warning" "$base" WYRD_VERIFY_SIGNATURE=skip GITHUB_STEP_SUMMARY="$tmp/summary"
contains "$LAST_OUT" "SIGNATURE VERIFICATION DISABLED" "loud warning in the log"
contains "$tmp/summary" "SIGNATURE VERIFICATION DISABLED" "warning in the job summary"

expect 0 "unsigned release, false (alias of skip): installed with a warning" "$base" WYRD_VERIFY_SIGNATURE=false
contains "$LAST_OUT" "SIGNATURE VERIFICATION DISABLED" "false warns like skip"

expect 1 "unsigned release, required: refused" "$base" WYRD_VERIFY_SIGNATURE=required
contains "$LAST_OUT" "verify-signature is required" "reason given"

expect 1 "bad verify-signature value" "$base" WYRD_VERIFY_SIGNATURE=maybe

fake_cosign 0
touch "$rel/SHA256SUMS.sigstore.json"
expect 0 "signed release: cosign verifies SHA256SUMS against the release workflow" "$base" WYRD_VERSION=v9.9.9
contains "$tmp/cosign.args" "verify-blob" "cosign verify-blob called"
contains "$tmp/cosign.args" "Kunj157/wyrdline/.github/workflows/release" "identity is the release workflow"
contains "$tmp/cosign.args" "token.actions.githubusercontent.com" "issuer is GitHub Actions"

fake_cosign 1
expect 1 "signed release, bad signature: refused" "$base"
rm -f "$tmp/bin/cosign"
expect 1 "signed release, no cosign: refused" "$base" PATH=/usr/bin:/bin
rm "$rel/SHA256SUMS.sigstore.json"

cp "$rel/SHA256SUMS" "$tmp/sums.bak"
awk '{ printf "%064d  %s\n", 0, $2 }' "$tmp/sums.bak" >"$rel/SHA256SUMS"
expect 1 "checksum mismatch: refused" "$base" WYRD_VERIFY_SIGNATURE=auto
contains "$LAST_OUT" "checksum mismatch" "reason given"
rm "$rel/SHA256SUMS"
expect 1 "no SHA256SUMS: refused" "$base" WYRD_VERIFY_SIGNATURE=auto
cp "$tmp/sums.bak" "$rel/SHA256SUMS"

expect 1 "missing release: refused" "WYRD_RELEASE_BASE=file://$tmp/nowhere"

expect 0 "prebuilt binaries via wyrd-path" "WYRD_PATH=$rel/pkg/wyrdline"
rm "$rel/pkg/wyrdline/wyrd-sensord"
expect 1 "wyrd-path without wyrd-sensord: refused" "WYRD_PATH=$rel/pkg/wyrdline"

echo "install.sh: $pass passed, $fail failed"
[ "$fail" = 0 ]
