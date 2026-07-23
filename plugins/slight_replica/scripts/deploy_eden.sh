#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$(dirname "$SCRIPT_DIR")"

NRO="target/aarch64-skyline-switch/release/lib_effect_viewer.nro"

if [ ! -f "$NRO" ]; then
    echo "NRO not found — run build.sh first"
    exit 1
fi

# Eden's normal mod-data directory. The directory directly below the title ID is
# the mod name (`Arcropolis`), followed by its LayeredFS `romfs` tree.
PLUGINS="$HOME/.local/share/eden/load/01006A800016E000/Arcropolis/romfs/skyline/plugins"

# Runtime Switch plugin dependencies that must be loaded alongside lib_effect_viewer.nro.
# The smashline-2 port REQUIRES libsmashline_plugin.nro (the smashline 2 engine that
# exports smashline_install_state_callback / smashline_install_line_callback), which in
# turn requires libnro_hook.nro. libarcropolis.nro provides romfs mod loading.
REQUIRED_DEPS=(libarcropolis.nro libnro_hook.nro libsmashline_plugin.nro)

mkdir -p "$PLUGINS"
# Remove old effect_viewer plugin (different name, would double-load hooks).
rm -f "$PLUGINS/libeffect_viewer.nro"
cp "$NRO" "$PLUGINS/lib_effect_viewer.nro"

echo "Deployed lib_effect_viewer.nro to:"
echo "  $PLUGINS/lib_effect_viewer.nro"

# Verify the runtime plugin dependencies are present in each load path.
MISSING=0
for dep in "${REQUIRED_DEPS[@]}"; do
    if [ ! -f "$PLUGINS/$dep" ]; then
        echo "  !! MISSING dependency: $PLUGINS/$dep"
        MISSING=1
    fi
done

if [ "$MISSING" -eq 0 ]; then
    echo "All runtime plugin dependencies present (${REQUIRED_DEPS[*]})."
    # Confirm the installed smashline plugin is v2 (exports the symbols our code calls).
    # (grep without -q so it consumes all of strings' output; -q would SIGPIPE strings
    #  and trip `set -o pipefail` into a false negative.)
    SL="$PLUGINS/libsmashline_plugin.nro"
    if [ -f "$SL" ] && ! strings -n 8 "$SL" 2>/dev/null | grep -F "smashline_install_state_callback" >/dev/null; then
        echo "  !! WARNING: $SL does not export smashline_install_state_callback —"
        echo "     it may be smashline 1. The smashline-2 build needs the smashline 2 plugin."
    fi
else
    echo "Install the missing plugins before launching, or the effect viewer will not hook."
fi

echo "Restart Eden fully so the new plugin loads."
