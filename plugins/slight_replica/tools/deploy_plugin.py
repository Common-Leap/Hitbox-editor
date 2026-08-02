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

    legacy = plugins / "libeffect_viewer.nro"
    if legacy.is_file():
        legacy.unlink()
    installed = plugins / "lib_effect_viewer.nro"
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
