#!/usr/bin/env bash
# Sets the workspace version to the release version so every binary reports
# it (`wyrd --version`), then refreshes the workspace entries in Cargo.lock
# offline. Dependencies are untouched and `--locked` builds still work.
#
# Usage: scripts/release/stamp-version.sh <version> [workspace-dir]
#        (e.g. 1.0.0, 1.0.0-rc.1; the workspace defaults to this repository)
set -euo pipefail

version=${1:?usage: stamp-version.sh <version> [workspace-dir]}
cd "${2:-$(dirname "$0")/../..}"
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "stamp-version: '$version' is not a semantic version" >&2; exit 1 ;;
esac

current=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version = "\(.*\)"$/\1/p}' Cargo.toml)
[ -n "$current" ] || { echo "stamp-version: no [workspace.package] version in Cargo.toml" >&2; exit 1; }
if [ "$current" = "$version" ]; then
  echo "stamp-version: workspace already at $version"
  exit 0
fi

# A clean runner has no registry index: fetch the locked packages first, so the
# offline update can resolve the workspace entries from the cache.
cargo fetch --locked --quiet
sed -i "/^\[workspace\.package\]/,/^\[/s/^version = \"$current\"$/version = \"$version\"/" Cargo.toml
cargo update --workspace --offline --quiet
cargo metadata --locked --offline --format-version 1 >/dev/null
echo "stamp-version: $current -> $version"
