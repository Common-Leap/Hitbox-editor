#!/usr/bin/env bash
# Deploy the plugin (+ its runtime deps and the skyline exefs) to Ryujinx.
# Mirrors the known-good Eden install at ~/.local/share/eden/load/01006A800016E000/Arcropolis.
#
# Ryujinx layout:
#   mods:   ~/.config/Ryujinx/mods/contents/01006A800016E000/Arcropolis/{exefs,romfs/...}
#   sdcard: ~/.config/Ryujinx/sdcard  → the plugin's sd:/ root
#           (diag file lands at ~/.config/Ryujinx/sdcard/slight/diag.txt)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$(dirname "$SCRIPT_DIR")"

NRO="target/aarch64-skyline-switch/release/lib_effect_viewer.nro"
if [ ! -f "$NRO" ]; then
    echo "NRO not found — run build.sh first"
    exit 1
fi

EDEN_MOD="$HOME/.local/share/eden/load/01006A800016E000/Arcropolis"
RYU_MOD="$HOME/.config/Ryujinx/mods/contents/01006A800016E000/Arcropolis"
PLUGINS="$RYU_MOD/romfs/skyline/plugins"

mkdir -p "$RYU_MOD/exefs" "$PLUGINS"

# Skyline loader exefs (subsdk9 + main.npdm) — required for any skyline plugin to load.
for f in subsdk9 main.npdm; do
    if [ -f "$EDEN_MOD/exefs/$f" ]; then
        cp "$EDEN_MOD/exefs/$f" "$RYU_MOD/exefs/$f"
    elif [ ! -f "$RYU_MOD/exefs/$f" ]; then
        echo "  !! MISSING skyline exefs: $f (not in Eden install either)"
    fi
done

# Runtime plugin dependencies (see deploy_eden.sh for why each is needed).
REQUIRED_DEPS=(libarcropolis.nro libnro_hook.nro libsmashline_plugin.nro)
for dep in "${REQUIRED_DEPS[@]}"; do
    if [ -f "$EDEN_MOD/romfs/skyline/plugins/$dep" ]; then
        cp "$EDEN_MOD/romfs/skyline/plugins/$dep" "$PLUGINS/$dep"
    elif [ ! -f "$PLUGINS/$dep" ]; then
        echo "  !! MISSING dependency: $dep (not in Eden install either)"
    fi
done

# Our plugin (remove the old-name variant that would double-load hooks).
rm -f "$PLUGINS/libeffect_viewer.nro"
cp "$NRO" "$PLUGINS/lib_effect_viewer.nro"

echo "Deployed to $RYU_MOD"
ls -la "$RYU_MOD/exefs" "$PLUGINS"
echo
echo "After running a match: diag file at ~/.config/Ryujinx/sdcard/slight/diag.txt"
