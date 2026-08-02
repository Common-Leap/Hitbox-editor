#!/usr/bin/env python3
"""Prepare Eden SD paths for Jorge-style SLight + RPM."""

from __future__ import annotations

import argparse
from pathlib import Path

from host_paths import eden_mod_directory, eden_sd_directory


def main() -> None:
    p = argparse.ArgumentParser(description="Create sd:/slight/ mirror dirs on Eden SDMC")
    p.add_argument(
        "--sdmc",
        type=Path,
        default=eden_sd_directory(),
        help="emulator SD root (default: VISIONARY_SD_DIR or the platform Eden location)",
    )
    p.add_argument("--host", default="127.0.0.1", help="RPM TCP host (written to gateway.txt)")
    p.add_argument("--port", type=int, default=7878)
    args = p.parse_args()

    sdmc = args.sdmc.expanduser().resolve()
    dirs = [
        sdmc / "slight" / "debug" / "loggers",
        sdmc / "slight" / "user" / "debuggables",
        sdmc / "slight" / "user" / "error_logs",
        sdmc / "slight" / "user",
    ]
    for d in dirs:
        d.mkdir(parents=True, exist_ok=True)

    gateway = sdmc / "slight" / "user" / "gateway.txt"
    gateway.write_text(f"{args.host}:{args.port}\n", encoding="utf-8")
    print(f"Wrote {gateway}")
    print(
        "Deploy NRO to "
        f"{eden_mod_directory() / 'romfs' / 'skyline' / 'plugins'}"
    )
    print("Connect RPM to the emulator IP on port", args.port)


if __name__ == "__main__":
    main()
