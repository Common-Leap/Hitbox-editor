"""Resolve reverse-engineering inputs from an external, untracked dump directory."""

from __future__ import annotations

import os
from pathlib import Path


def dump_file(relative_path: str) -> Path:
    root = os.environ.get("SSBU_DUMP_DIR")
    if not root:
        raise RuntimeError(
            "SSBU_DUMP_DIR is not set; point it at your external SSBU dump directory"
        )
    return Path(root).expanduser().resolve() / relative_path
