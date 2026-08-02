#!/usr/bin/env python3
"""Build slight_replica on Windows, Linux, or macOS."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "cargo_args",
        nargs=argparse.REMAINDER,
        help="additional arguments passed to cargo skyline build",
    )
    args = parser.parse_args()

    plugin_root = Path(__file__).resolve().parents[1]
    environment = os.environ.copy()
    target_value = environment.get("CARGO_TARGET_DIR")
    target_dir = Path(target_value) if target_value else plugin_root / "target"
    if not target_dir.is_absolute():
        target_dir = plugin_root / target_dir
    environment["CARGO_TARGET_DIR"] = str(target_dir)

    gc_flag = "-C link-arg=-Wl,--gc-sections"
    rustflags = environment.get("RUSTFLAGS", "").strip()
    if gc_flag not in rustflags:
        environment["RUSTFLAGS"] = f"{rustflags} {gc_flag}".strip()

    command = ["cargo", "skyline", "build", "--release", *args.cargo_args]
    subprocess.run(command, cwd=plugin_root, env=environment, check=True)

    nro = target_dir / "aarch64-skyline-switch" / "release" / "lib_effect_viewer.nro"
    if not nro.is_file():
        raise SystemExit(f"Build completed but the plugin was not found at {nro}")
    output = target_dir / "output"
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(nro, output / nro.name)
    print(f"Built: {nro} ({nro.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
