#!/bin/bash
# Run the app and capture TEX/RENDER diagnostic logs
# Usage: ./run_tex_diag.sh 2>&1 | grep -E '\[TEX\]|\[RENDER\]|\[BNTX\]|\[GRTF\]' | head -100
cargo build 2>&1 | grep -E "^error" && exit 1
echo "Build OK — run the app and pipe stderr through:"
echo "  ./hitbox_editor 2>&1 | grep -E '\[TEX\]|\[RENDER\] WARN' | head -50"
