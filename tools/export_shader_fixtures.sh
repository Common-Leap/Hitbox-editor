#!/usr/bin/env bash
# Sync gitignored BNSH fixtures under tests/fixtures/shaders/.
#
# Tests call ensure_shader_fixtures() automatically; use this script to
# populate fixtures manually (same logic as the Rust helper).
#
# Usage:
#   ./tools/export_shader_fixtures.sh [fighter]
#
# Requires editor data_root or HITBOX_EFFECT_EXPORT (and optionally PTCL dump cache).

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIGHTER="${1:-samus}"

cd "$ROOT"
cargo test --lib "bnsh_shader_integration::tests::test_sync_shader_fixtures_from_export" -- --nocapture 2>&1 \
  | rg '^\[FIXTURE\]|Skipping:|test_sync_shader_fixtures_from_export' || true

echo "Fixtures dir: $ROOT/tests/fixtures/shaders"
ls -1 "$ROOT/tests/fixtures/shaders/"*.bnsh 2>/dev/null | wc -l | xargs -I{} echo "{} .bnsh file(s)"
