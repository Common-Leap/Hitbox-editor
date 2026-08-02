"""Portable host paths used by the slight_replica helper tools.

Every location can be overridden so portable emulator installations do not need
to match an operating system's conventional data directories.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

TITLE_ID = "01006A800016E000"
MOD_NAME = "Arcropolis"


def _environment_path(name: str) -> Path | None:
    value = os.environ.get(name)
    if not value:
        return None
    return Path(value).expanduser()


def data_directory() -> Path:
    """Return the current platform's per-user application-data directory."""
    override = _environment_path("XDG_DATA_HOME")
    if override is not None and sys.platform != "win32":
        return override
    if sys.platform == "win32":
        value = os.environ.get("APPDATA") or os.environ.get("LOCALAPPDATA")
        if value:
            return Path(value)
        return Path.home() / "AppData" / "Roaming"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support"
    return Path.home() / ".local" / "share"


def config_directory() -> Path:
    """Return the current platform's per-user configuration directory."""
    override = _environment_path("XDG_CONFIG_HOME")
    if override is not None and sys.platform != "win32":
        return override
    if sys.platform == "win32":
        value = os.environ.get("APPDATA") or os.environ.get("LOCALAPPDATA")
        if value:
            return Path(value)
        return Path.home() / "AppData" / "Roaming"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support"
    return Path.home() / ".config"


def eden_mod_directory() -> Path:
    return _environment_path("VISIONARY_EDEN_MOD_DIR") or (
        data_directory() / "eden" / "load" / TITLE_ID / MOD_NAME
    )


def eden_sd_directory() -> Path:
    return _environment_path("VISIONARY_SD_DIR") or data_directory() / "eden" / "sdmc"


def ryujinx_mod_directory() -> Path:
    return _environment_path("VISIONARY_RYUJINX_MOD_DIR") or (
        config_directory() / "Ryujinx" / "mods" / "contents" / TITLE_ID / MOD_NAME
    )


def ryujinx_sd_directory() -> Path:
    return _environment_path("VISIONARY_RYUJINX_SD_DIR") or (
        config_directory() / "Ryujinx" / "sdcard"
    )
