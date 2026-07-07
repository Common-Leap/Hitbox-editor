#!/usr/bin/env bash
# Seed / refresh gitignored regression goldens under tests/goldens/.
#
# Writes the CURRENT editor render for each harness case (see tests/regression_harness.rs)
# as the golden baseline. Two uses:
#   1. Arm the regression gate against your machine's output (pure regression detection).
#   2. Regenerate any goldens still missing after you've dropped in framing-matched real
#      game frames for accuracy comparison.
#
# Usage:
#   ./tools/export_goldens.sh
#
# Requires a GPU and editor data_root or HITBOX_EFFECT_EXPORT. The toolchain is pinned by
# rust-toolchain.toml, so plain `cargo` uses nightly-2026-02-14 automatically.

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

UPDATE_GOLDENS=1 cargo test --test regression_harness -- --test-threads=1 --nocapture 2>&1 \
  | rg '^\[regression\]|test result' || true

echo "Goldens dir: $ROOT/tests/goldens"
find "$ROOT/tests/goldens" -name '*.png' 2>/dev/null | wc -l | xargs -I{} echo "{} golden PNG(s)"
