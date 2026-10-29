#!/usr/bin/env bash
# Reproducible-build check: compares every file of directory A with the
# same-named file in directory B by SHA-256. Prints one line per file and
# exits 1 if any file differs or is missing, naming each one.
#
# Usage: scripts/release/compare-builds.sh <dir-a> <dir-b> [label-a] [label-b]
set -uo pipefail

a=${1:?usage: compare-builds.sh <dir-a> <dir-b> [label-a] [label-b]}
b=${2:?usage: compare-builds.sh <dir-a> <dir-b> [label-a] [label-b]}
la=${3:-$a}
lb=${4:-$b}
[ -d "$a" ] || { echo "compare-builds: $a is not a directory" >&2; exit 2; }
[ -d "$b" ] || { echo "compare-builds: $b is not a directory" >&2; exit 2; }

differing=()
checked=0
while IFS= read -r rel; do
  checked=$((checked + 1))
  if [ ! -f "$b/$rel" ]; then
    echo "MISSING  $rel (not in $lb)"
    differing+=("$rel")
    continue
  fi
  ha=$(sha256sum "$a/$rel" | cut -d' ' -f1)
  hb=$(sha256sum "$b/$rel" | cut -d' ' -f1)
  if [ "$ha" = "$hb" ]; then
    echo "same     $rel  $ha"
  else
    echo "DIFFERS  $rel"
    echo "         $la: $ha ($(stat -c %s "$a/$rel") bytes)"
    echo "         $lb: $hb ($(stat -c %s "$b/$rel") bytes)"
    differing+=("$rel")
  fi
done < <(cd "$a" && find . -type f | sed 's#^\./##' | LC_ALL=C sort)

if [ "$checked" = 0 ]; then
  echo "compare-builds: nothing to compare in $a" >&2
  exit 2
fi
if [ "${#differing[@]}" != 0 ]; then
  echo "compare-builds: ${#differing[@]} of $checked files differ between $la and $lb: ${differing[*]}" >&2
  exit 1
fi
echo "compare-builds: all $checked files identical"
