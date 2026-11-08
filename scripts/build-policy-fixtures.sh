#!/usr/bin/env bash
# Rebuild the committed test guests in policies/fixtures/ from
# policies/examples/. The host tests load these files, so they run without
# a wasm32-wasip2 build in every test run.
set -euo pipefail
cd "$(dirname "$0")/.."
# The policies workspace builds into policies/target/ unless CARGO_TARGET_DIR is set.
target_dir=${CARGO_TARGET_DIR:-policies/target}
cargo build --quiet --locked --release --target wasm32-wasip2 \
  --manifest-path policies/Cargo.toml -p hostile -p escape
mkdir -p policies/fixtures
for p in hostile escape; do
  cp "$target_dir/wasm32-wasip2/release/$p.wasm" "policies/fixtures/$p.wasm"
done
ls -l policies/fixtures/
