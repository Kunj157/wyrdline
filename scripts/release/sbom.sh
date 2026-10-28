#!/usr/bin/env bash
# CycloneDX 1.5 SBOM (JSON) for each shipped binary, per target, from the
# locked dependency graph. Needs cargo-cyclonedx.
#
# Usage: scripts/release/sbom.sh --out <dir> [--target <triple>]...
# Output: <out>/wyrdline-<binary>-<target>.cdx.json
set -euo pipefail
cd "$(dirname "$0")/../.."

out=''
targets=()
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out=$2; shift 2 ;;
    --target) targets+=("$2"); shift 2 ;;
    *) echo "sbom: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] || { echo "usage: sbom.sh --out <dir> [--target <triple>]..." >&2; exit 2; }
[ "${#targets[@]}" != 0 ] || targets=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)
mkdir -p "$out"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}

cleanup() { find crates -maxdepth 2 -name '*.cdx.json' -delete; }
trap cleanup EXIT
for target in "${targets[@]}"; do
  cleanup
  cargo cyclonedx --quiet --format json --spec-version 1.5 --describe binaries --all \
    --target "$target" --target-in-filename
  for pair in "wyrd:wyrd-cli" "wyrdd:wyrdd" "wyrd-sensord:wyrd-sensord" "wyrd-hub:wyrd-hub"; do
    bin=${pair%%:*}
    crate=${pair##*:}
    src="crates/$crate/${bin}_bin_${target}.cdx.json"
    [ -f "$src" ] || { echo "sbom: cargo-cyclonedx produced no $src" >&2; exit 1; }
    mv "$src" "$out/wyrdline-$bin-$target.cdx.json"
  done
done
ls -1 "$out"
