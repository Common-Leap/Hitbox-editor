#!/usr/bin/env python3
"""Deploy slight_replica to a declared Eden or Ryujinx mod directory."""

from __future__ import annotations

import argparse
import os
import shutil
from pathlib import Path

from host_paths import (
    eden_mod_directory,
    ryujinx_mod_directory,
    ryujinx_sd_directory,
)

RUNTIME_DEPENDENCIES = (
    "libarcropolis.nro",
    "libnro_hook.nro",
    "libsmashline_plugin.nro",
)
SKYLINE_EXEFS = ("subsdk9", "main.npdm")
INSTALLED_NAME = "lib_effect_viewer.nro"
LEGACY_NAME = "libeffect_viewer.nro"

# Any file in the plugins directory whose bytes contain this is a copy of us.
#
# Skyline loads EVERY file in romfs:/skyline/plugins/ as a plugin, extension ignored. A
# `lib_effect_viewer.nro.bak` left beside the real one therefore runs a second full copy: two sets
# of ACMD hooks, two per-frame drivers, two servers racing for :7878, both writing the same
# sd:/slight/ diag files. It presents as a hard 60->30 fps drop on entering training mode, which
# looks like a performance regression in whatever was last changed and is not one. It also
# invalidates every A/B test run while it is there, which is how it cost ~6 rounds once.
#
# Matched on content, not on filename, because the name is exactly what varies -- `.bak`, `.old`,
# `lib_effect_viewer (1).nro`, a hand-renamed known-good build. It is the path constant from the
# plugin's diag module, which every build compiles into rodata; `tests/deploy_plugin.rs` pins that
# the string is still in the source it comes from.
#
# It also matches upstream Jorge SLight builds, and that is deliberate rather than a false
# positive: they bind the same port and write the same files, so having one installed alongside
# this plugin is the same conflict under a different name.
PLUGIN_MARKER = b"sd:/slight/diag.txt"


def is_plugin_copy(path: Path) -> bool:
    """True when `path` is some build of this plugin, whatever it has been renamed to."""
    try:
        return PLUGIN_MARKER in path.read_bytes()
    except OSError:
        return False


def scan_plugins_dir(plugins: Path) -> tuple[list[Path], list[Path]]:
    """Split the files in `plugins` into (strays, unrecognised).

    A stray is a second copy of this plugin under any name. Unrecognised files are reported but
    never block a deploy -- an unrelated Skyline plugin living in the same directory is a normal,
    supported thing to have, and refusing to deploy over one would be wrong.
    """
    known = {INSTALLED_NAME, LEGACY_NAME, *RUNTIME_DEPENDENCIES}
    strays: list[Path] = []
    unrecognised: list[Path] = []
    for entry in sorted(plugins.iterdir()):
        if not entry.is_file() or entry.name in known:
            continue
        (strays if is_plugin_copy(entry) else unrecognised).append(entry)
    return strays, unrecognised


def default_nro() -> Path:
    plugin_root = Path(__file__).resolve().parents[1]
    target_value = os.environ.get("CARGO_TARGET_DIR")
    target = Path(target_value) if target_value else plugin_root / "target"
    if not target.is_absolute():
        target = plugin_root / target
    return target / "aarch64-skyline-switch" / "release" / "lib_effect_viewer.nro"


def copy_missing(source: Path | None, destination: Path, names: tuple[str, ...]) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for name in names:
        target = destination / name
        candidate = source / name if source is not None else None
        if candidate is not None and candidate.is_file():
            shutil.copy2(candidate, target)
        elif not target.is_file():
            print(f"  !! MISSING dependency: {target}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--emulator", choices=("eden", "ryujinx"), required=True)
    parser.add_argument(
        "--mod-dir",
        type=Path,
        help=(
            "Arcropolis mod root; overrides VISIONARY_EDEN_MOD_DIR or "
            "VISIONARY_RYUJINX_MOD_DIR"
        ),
    )
    parser.add_argument(
        "--source-mod-dir",
        type=Path,
        help="optional existing mod root from which Ryujinx dependencies are copied",
    )
    parser.add_argument("--nro", type=Path, default=default_nro())
    parser.add_argument(
        "--remove-strays",
        action="store_true",
        help="delete other copies of this plugin found in the target directory",
    )
    args = parser.parse_args()

    mod_dir = args.mod_dir or (
        eden_mod_directory() if args.emulator == "eden" else ryujinx_mod_directory()
    )
    mod_dir = mod_dir.expanduser().resolve()
    nro = args.nro.expanduser().resolve()
    if not nro.is_file():
        raise SystemExit(f"Plugin not found at {nro}; build it first")

    source_mod = args.source_mod_dir
    if source_mod is None and args.emulator == "ryujinx":
        source_mod = eden_mod_directory()
    if source_mod is not None:
        source_mod = source_mod.expanduser().resolve()

    plugins = mod_dir / "romfs" / "skyline" / "plugins"
    plugins.mkdir(parents=True, exist_ok=True)
    if args.emulator == "ryujinx":
        source_exefs = source_mod / "exefs" if source_mod and source_mod.is_dir() else None
        copy_missing(source_exefs, mod_dir / "exefs", SKYLINE_EXEFS)
        source_plugins = (
            source_mod / "romfs" / "skyline" / "plugins"
            if source_mod and source_mod.is_dir()
            else None
        )
        copy_missing(source_plugins, plugins, RUNTIME_DEPENDENCIES)

    legacy = plugins / LEGACY_NAME
    if legacy.is_file():
        legacy.unlink()

    strays, unrecognised = scan_plugins_dir(plugins)
    for entry in unrecognised:
        print(f"  -- not one of ours, left alone: {entry.name}")
    if strays and not args.remove_strays:
        # Refuse BEFORE copying, so a refusal changes nothing and can be re-run after the user
        # has looked at the files themselves. Deleting anything in someone's mod directory
        # without being asked is not this script's call to make -- one of these is quite
        # plausibly a known-good build somebody parked there on purpose.
        print()
        print("REFUSING TO DEPLOY: a second copy of this plugin is already installed.")
        for entry in strays:
            print(f"  {entry}")
        print()
        print(
            "Skyline loads every file in this directory as a plugin, extension ignored, so each\n"
            "of the above runs a second full copy: double ACMD hooks, double per-frame ticks,\n"
            "two servers fighting over :7878. It shows up as a 60->30 fps drop in training mode\n"
            "and it invalidates any A/B test run while it is there."
        )
        print()
        print("Move them somewhere outside this directory, or re-run with --remove-strays.")
        raise SystemExit(2)
    for entry in strays:
        entry.unlink()
        print(f"  Removed second plugin copy: {entry}")

    installed = plugins / INSTALLED_NAME
    shutil.copy2(nro, installed)
    print(f"Deployed {installed}")

    missing = [name for name in RUNTIME_DEPENDENCIES if not (plugins / name).is_file()]
    for name in missing:
        print(f"  !! MISSING dependency: {plugins / name}")
    if not missing:
        print("All runtime plugin dependencies are present.")
        smashline = plugins / "libsmashline_plugin.nro"
        if b"smashline_install_state_callback" not in smashline.read_bytes():
            print(
                "  !! WARNING: libsmashline_plugin.nro may be Smashline 1; "
                "the Smashline 2 callback export was not found."
            )

    if args.emulator == "ryujinx":
        print(f"Runtime diagnostics: {ryujinx_sd_directory() / 'slight' / 'diag.txt'}")
    print(f"Restart {args.emulator.title()} fully so the new plugin loads.")


if __name__ == "__main__":
    main()
