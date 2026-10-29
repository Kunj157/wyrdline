#!/usr/bin/env bash
# Sets the Hub chart's version and appVersion to the release version, so a
# release's chart installs the image that release published (the image is
# tagged with the version without its `v`, see .github/workflows/hub-image.yml).
# The chart's appVersion is the image tag its default points at.
#
# Usage: scripts/release/stamp-chart.sh <version> [chart-dir]
#        (e.g. 1.0.0, 1.0.0-rc.1; the chart defaults to deploy/helm/wyrdline-hub)
set -euo pipefail

version=${1:?usage: stamp-chart.sh <version> [chart-dir]}
dir=${2:-$(dirname "$0")/../../deploy/helm/wyrdline-hub}
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "stamp-chart: '$version' is not a semantic version" >&2; exit 1 ;;
esac
chart="$dir/Chart.yaml"
[ -f "$chart" ] || { echo "stamp-chart: no $chart" >&2; exit 1; }

sed -i \
  -e "s/^version: .*\$/version: $version/" \
  -e "s/^appVersion: .*\$/appVersion: \"$version\"/" \
  "$chart"
if ! grep -q "^version: $version\$" "$chart" || ! grep -q "^appVersion: \"$version\"\$" "$chart"; then
  echo "stamp-chart: could not stamp $chart" >&2
  exit 1
fi
echo "stamp-chart: chart and appVersion $version"
