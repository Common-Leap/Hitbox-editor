#!/usr/bin/env bash
# Full shader/particle cache reset + rebuild. See build.rs header for details.
set -euo pipefail
cd "$(dirname "$0")/.."
export CLEAN_SHADERS=1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
exec cargo build "$@"
