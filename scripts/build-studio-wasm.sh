#!/usr/bin/env bash
# Builds the WebAssembly correlator for the Studio's offline mode into
# apps/studio/src/wasm (generated, not committed). Needs the
# wasm32-unknown-unknown target and wasm-bindgen-cli matching wyrd-wasm.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --quiet --locked --release -p wyrd-wasm --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir apps/studio/src/wasm target/wasm32-unknown-unknown/release/wyrd_wasm.wasm
