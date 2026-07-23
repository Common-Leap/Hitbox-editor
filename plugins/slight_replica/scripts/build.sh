#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$(dirname "$SCRIPT_DIR")"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$(pwd)/target}"
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,--gc-sections"
cargo skyline build --release
NRO="$CARGO_TARGET_DIR/aarch64-skyline-switch/release/lib_effect_viewer.nro"
mkdir -p "$CARGO_TARGET_DIR/output"
cp "$NRO" "$CARGO_TARGET_DIR/output/"
echo "Built: $NRO ($(wc -c < "$NRO") bytes)"
