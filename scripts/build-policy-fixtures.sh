#!/usr/bin/env bash
# Rebuild the committed test guests in policies/fixtures/ from
# policies/examples/. The host tests load these files, so they run without
# a wasm32-wasip2 build in every test run.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --quiet --locked --release --target wasm32-wasip2 \
  --manifest-path policies/Cargo.toml -p hostile
mkdir -p policies/fixtures
cp policies/target/wasm32-wasip2/release/hostile.wasm policies/fixtures/hostile.wasm
ls -l policies/fixtures/
