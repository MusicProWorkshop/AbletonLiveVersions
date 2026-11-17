"""Version naming ("Song v_7.als"), finding sibling versions, and the notes files."""
import datetime
import os
import re
from dataclasses import dataclass

from . import als, changes, config

# "<base><sep>v_<n>" where sep is a space, underscore or dash (or nothing).
VERSION_RE = re.compile(r"^(?P<base>.*?)(?P<sep>[ _-]?)v_(?P<num>\d+)$", re.IGNORECASE)
NOTES_MARKER = "--- Your notes (kept when this file is regenerated) ---"


@dataclass
class Version:
    path: str
    base: str
    num: int          # an unnumbered set counts as version 1
    sep: str

    @property
    def stem(self) -> str:
        return os.path.splitext(os.path.basename(self.path))[0]

    @property
    def label(self) -> str:
        return f"v_{self.num}" if VERSION_RE.match(self.stem) else "original"


def parse(path: str) -> Version:
    stem = os.path.splitext(os.path.basename(path))[0]
    m = VERSION_RE.match(stem)
    if m:
        return Version(path, m.group("base"), int(m.group("num")), m.group("sep"))
    return Version(path, stem, 1, " ")


def family(path: str) -> list:
    """All versions of the same set in its folder, oldest first."""
    me = parse(path)
    folder = os.path.dirname(os.path.abspath(path))
    out = []
    for f in os.listdir(folder):
        if not f.lower().endswith(".als"):
            continue
        v = parse(os.path.join(folder, f))
        if v.base.strip().lower() == me.base.strip().lower():
            out.append(v)
    out.sort(key=lambda v: (v.num, os.path.getmtime(v.path)))
    return out


def families_in(folder: str) -> dict:
    """{base name: [versions]} for every set in a project folder."""
    out = {}
    for f in sorted(os.listdir(folder)):
        if f.lower().endswith(".als"):
            v = parse(os.path.join(folder, f))
            out.setdefault(v.base.strip(), []).append(v)
    for vs in out.values():
        vs.sort(key=lambda v: v.num)
    return out


def next_version_path(path: str) -> str:
    """Next free number after the highest existing one, so saving from an old
    version (v_3 while v_6 exists) creates v_7 instead of overwriting v_4."""
    me = parse(path)
    top = max([v.num for v in family(path)] + [me.num])
    sep = me.sep if VERSION_RE.match(me.stem) else " "
    folder = os.path.dirname(os.path.abspath(path))
    n = top + 1
    while True:
        candidate = os.path.join(folder, f"{me.base}{sep}v_{n}.als")
        if not os.path.exists(candidate):
            return candidate
        n += 1


def previous_version(path: str):
    """The version this one was saved from (recorded in its notes) or else the
    closest lower number."""
    rec = read_notes(path).get("parent")
    if rec:
        p = os.path.join(os.path.dirname(path), rec)
        if os.path.exists(p):
            return p
    me = parse(path)
    lower = [v for v in family(path) if v.num < me.num]
    return lower[-1].path if lower else None


# ----------------------------------------------------------------------------- notes files

def notes_path(als_path: str, cfg=None) -> str:
    cfg = cfg or config.load()
    folder, name = os.path.split(os.path.abspath(als_path))
    stem = os.path.splitext(name)[0]
    if cfg.get("notes_location") == "beside":
        return os.path.join(folder, f"{stem} - changes.txt")
    return os.path.join(folder, cfg.get("notes_subfolder") or "Version Notes", f"{stem}.txt")


def read_notes(als_path: str) -> dict:
    """{'text': generated part, 'user': user notes, 'parent': file name or None}"""
    p = notes_path(als_path)
    try:
        with open(p, encoding="utf-8") as f:
            content = f.read()
    except OSError:
        return {"text": "", "user": "", "parent": None, "exists": False}
    gen, _, user = content.partition(NOTES_MARKER)
    m = re.search(r"^Compared with: (.+\.als)\s*$", gen, re.MULTILINE)
    # Extra block (e.g. a VST3 conversion report) sits between the overview line and "Summary:".
    x = re.search(r"^Ableton Live[^\n]*\n\n(.*?)\n\nSummary:", gen, re.MULTILINE | re.DOTALL)
    return {"text": gen.rstrip(), "user": user.strip("\n"), "parent": m.group(1) if m else None,
            "extra": x.group(1).splitlines() if x else None, "exists": True}


def write_notes(als_path: str, parent_path=None, extra_lines=None, user_text=None) -> str:
    """(Re)generate the change notes for a version. Returns the summary line."""
    old = read_notes(als_path)
    if extra_lines is None:
        extra_lines = old.get("extra")
    if parent_path is None:
        parent_path = previous_version(als_path)
    new_snap = als.snapshot(als_path)
    old_snap = als.snapshot(parent_path) if parent_path else None
    ch = changes.diff(old_snap, new_snap) if old_snap else None
    saved = datetime.datetime.fromtimestamp(os.path.getmtime(als_path))
    stem = os.path.splitext(os.path.basename(als_path))[0]

    lines = [f"{stem}", "=" * len(stem), f"Saved: {saved:%Y-%m-%d %H:%M}"]
    if parent_path:
        lines.append(f"Compared with: {os.path.basename(parent_path)}")
    lines.append(changes.set_overview(new_snap))
    lines.append("")
    if extra_lines:
        lines += list(extra_lines) + [""]
    if ch is None:
        summary = "First version · " + changes.counts_line(new_snap)
        lines += [f"Summary: {summary}", "", "Contents:"]
        lines += changes.contents_lines(new_snap)
    else:
        summary = changes.summary_line(ch)
        lines.append(f"Summary: {summary}")
        lines.append("")
        lines += changes.format_lines(ch)
    user = old["user"] if user_text is None else user_text
    lines += ["", "", NOTES_MARKER, user or ""]
    p = notes_path(als_path)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w", encoding="utf-8", newline="\r\n" if os.name == "nt" else "\n") as f:
        f.write("\n".join(lines).rstrip() + "\n")
    return summary


def save_user_note(als_path: str, text: str) -> None:
    n = read_notes(als_path)
    if not n["exists"]:
        write_notes(als_path, user_text=text)
        return
    p = notes_path(als_path)
    with open(p, "w", encoding="utf-8", newline="\r\n" if os.name == "nt" else "\n") as f:
        f.write(n["text"] + "\n\n\n" + NOTES_MARKER + "\n" + text.strip() + "\n")


def summary_from_notes(als_path: str):
    n = read_notes(als_path)
    m = re.search(r"^Summary: (.+)$", n["text"], re.MULTILINE)
    if m:
        return m.group(1).strip()
    if "First version" in n["text"]:
        return "First version"
    return None
