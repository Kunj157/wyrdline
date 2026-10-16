#!/usr/bin/env bash
# Build the landing page with the real Studio inside it, as published to
# GitHub Pages: apps/site/dist/ with Studio under studio/ replaying the
# recorded demo session offline. WYRD_SITE_BASE sets the URL prefix.
set -euo pipefail
cd "$(dirname "$0")/.."

base="${WYRD_SITE_BASE:-/wyrdline/}"

# The offline demo needs the WebAssembly correlator inside Studio.
[ -f apps/studio/src/wasm/wyrd_wasm.js ] || scripts/build-studio-wasm.sh

pnpm --filter @wyrdline/studio exec vite build --base "${base}studio/" --outDir dist-site --emptyOutDir
WYRD_SITE_BASE="$base" pnpm --filter @wyrdline/site build

rm -rf apps/site/dist/studio
mv apps/studio/dist-site apps/site/dist/studio
cp demo/recordings/demo-session.jsonl apps/site/dist/studio/demo-session.jsonl
echo "site built in apps/site/dist (base $base)"
