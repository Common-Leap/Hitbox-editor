#!/usr/bin/env python3
"""Prepare Eden SD paths for Jorge-style SLight + RPM."""

from __future__ import annotations

import argparse
from pathlib import Path

DEFAULT_SDMC = Path.home() / ".local/share/eden/sdmc"


def main() -> None:
    p = argparse.ArgumentParser(description="Create sd:/slight/ mirror dirs on Eden SDMC")
    p.add_argument("--sdmc", type=Path, default=DEFAULT_SDMC)
    p.add_argument("--host", default="127.0.0.1", help="RPM TCP host (written to gateway.txt)")
    p.add_argument("--port", type=int, default=7878)
    args = p.parse_args()

    dirs = [
        args.sdmc / "slight/debug/loggers",
        args.sdmc / "slight/user/debuggables",
        args.sdmc / "slight/user/error_logs",
        args.sdmc / "slight/user",
    ]
    for d in dirs:
        d.mkdir(parents=True, exist_ok=True)

    gateway = args.sdmc / "slight/user/gateway.txt"
    gateway.write_text(f"{args.host}:{args.port}\n")
    print(f"Wrote {gateway}")
    print(
        "Deploy NRO to "
        "~/.local/share/eden/load/01006A800016E000/"
        "Arcropolis/romfs/skyline/plugins/"
    )
    print("Connect RPM to the emulator IP on port", args.port)


if __name__ == "__main__":
    main()
