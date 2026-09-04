#!/usr/bin/env bash
# watch_plugin.sh — rebuild and redeploy the Skyline plugin whenever its sources change.
# Run this in a terminal alongside Eden + Visionary for hands-free testing:
#   bash scripts/watch_plugin.sh
# It polls every 2s, so no inotify/cargo-watch needed. Ctrl-C to stop.

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLUGIN="$ROOT/plugins/slight_replica"
NRO="$PLUGIN/target/aarch64-skyline-switch/release/lib_effect_viewer.nro"

echo "[watch] Monitoring $PLUGIN/src for changes (poll 2s). Ctrl-C to stop."
# Touch a marker so the first iteration always builds if NRO is missing or stale.
LAST=""
while true; do
  # Hash all plugin sources + Cargo.toml
  CUR=$(find "$PLUGIN/src" "$PLUGIN/Cargo.toml" -type f -exec sha1sum {} \; 2>/dev/null | sha1sum | cut -d' ' -f1)
  if [[ "$CUR" != "$LAST" ]]; then
    echo "[watch] Change detected ($CUR) — rebuilding..."
    if bash "$PLUGIN/scripts/build.sh" 2>&1 | tail -n 30; then
      echo "[watch] Build+deploy ok at $(date +%H:%M:%S) — restart Eden to reload the NRO."
    else
      echo "[watch] Build failed at $(date +%H:%M:%S) — see above. Will retry on next change."
    fi
    LAST="$CUR"
  fi
  sleep 2
done
