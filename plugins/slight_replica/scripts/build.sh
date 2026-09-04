#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 "$SCRIPT_DIR/../tools/build_plugin.py" "$@"
# Auto-deploy to the local Eden install so the user only has to run Eden + Visionary.
# This is best-effort: if no Eden SD is found or the deploy is blocked by a stray
# copy, the build still succeeded and the deploy script's message explains why.
if python3 "$SCRIPT_DIR/../tools/deploy_plugin.py" --emulator eden 2>&1 | tee /tmp/visionary_deploy.log; then
  echo "[visionary] Plugin auto-deployed to Eden."
else
  echo "[visionary] Auto-deploy skipped or blocked — see /tmp/visionary_deploy.log (run with --remove-strays if needed)."
fi
