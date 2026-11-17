"""User settings, stored as JSON in the per-user app-data folder."""
import json
import os
import sys

APP_NAME = "LiveVersions"

DEFAULTS = {
    # Global hotkeys (only act while Ableton Live is the active window).
    "hotkey_save_version": "ctrl+alt+s",
    "hotkey_open_browser": "ctrl+alt+v",
    # Where change notes go: "subfolder" = <Project>/Version Notes/<set>.txt,
    # "beside" = next to the .als as "<set> - changes.txt".
    "notes_location": "subfolder",
    "notes_subfolder": "Version Notes",
    # Write notes for older versions that have none when the browser opens them.
    "auto_write_missing_notes": True,
    # Seconds to wait for Live to finish writing a big set after Save As.
    "save_timeout_seconds": 120,
    # Extra folders scanned by "learn" to discover VST3 parameter IDs.
    "learn_folders": [],
}


def app_dir() -> str:
    if sys.platform == "win32":
        base = os.environ.get("APPDATA") or os.path.expanduser("~")
    elif sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    path = os.path.join(base, APP_NAME)
    os.makedirs(path, exist_ok=True)
    return path


def cache_dir() -> str:
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or app_dir()
        path = os.path.join(base, APP_NAME, "cache")
    else:
        path = os.path.join(app_dir(), "cache")
    os.makedirs(path, exist_ok=True)
    return path


def config_path() -> str:
    return os.path.join(app_dir(), "config.json")


def load() -> dict:
    cfg = dict(DEFAULTS)
    try:
        with open(config_path(), encoding="utf-8") as f:
            cfg.update(json.load(f))
    except FileNotFoundError:
        save(cfg)
    except (OSError, ValueError):
        pass
    return cfg


def save(cfg: dict) -> None:
    with open(config_path(), "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2)
