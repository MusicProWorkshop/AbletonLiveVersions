"""Compare two snapshots and describe the changes that matter.

Deliberately ignored: clip contents, automation curves, small mixer moves, track
colours, device parameter tweaks. Those change on nearly every save and would
bury the interesting parts (new tracks, instruments, devices, structure).
"""
from collections import Counter

MAX_LIST = 8


def _q(name):
    return f'"{name}"'


def _inst(t):
    return ", ".join(t["instruments"]) if t["instruments"] else ""


def _match_tracks(old, new):
    """Pair tracks by Live's persistent track Id, then by type+name for the rest."""
    pairs, by_id = [], {t["id"]: t for t in old}
    used_old, unmatched_new = set(), []
    for t in new:
        o = by_id.get(t["id"])
        if o is not None and o["type"] == t["type"] and id(o) not in used_old:
            pairs.append((o, t))
            used_old.add(id(o))
        else:
            unmatched_new.append(t)
    rest_old = [o for o in old if id(o) not in used_old]
    still_new = []
    for t in unmatched_new:
        o = next((o for o in rest_old if o["type"] == t["type"]
                  and o.get("ident", o["name"]) == t.get("ident", t["name"])), None)
        if o is not None:
            rest_old.remove(o)
            pairs.append((o, t))
        else:
            still_new.append(t)
    return pairs, still_new, rest_old


def diff(old: dict, new: dict) -> dict:
    ch = {k: [] for k in ("set", "added", "removed", "renamed", "instruments", "devices",
                          "groups", "state", "clips", "samples")}
    if old.get("tempo") != new.get("tempo") and new.get("tempo") is not None:
        ch["set"].append(f"Tempo {old.get('tempo')} → {new.get('tempo')} BPM")
    # Only when Scale mode is on in both: toggling it on/off is not a key change.
    if old.get("key") and new.get("key") and old["key"] != new["key"]:
        ch["set"].append(f"Key {old['key']} → {new['key']}")
    if old.get("scenes") != new.get("scenes"):
        ch["set"].append(f"Scenes {old.get('scenes')} → {new.get('scenes')}")
    ob, nb = old.get("length_beats", 0) / 4, new.get("length_beats", 0) / 4
    if abs(nb - ob) >= 1:
        ch["set"].append(f"Arrangement length {ob:g} → {nb:g} bars")
    lo, ln = Counter(old.get("locators", [])), Counter(new.get("locators", []))
    if lo != ln:
        added = list((ln - lo).elements())
        gone = list((lo - ln).elements())
        parts = ([f"+ {_q(x)}" for x in added] + [f"− {_q(x)}" for x in gone])[:MAX_LIST]
        ch["set"].append("Locators: " + ", ".join(parts))
    if old.get("creator") != new.get("creator"):
        ch["set"].append(f"Saved with {new.get('creator')} (was {old.get('creator')})")

    pairs, added, removed = _match_tracks(old["tracks"], new["tracks"])
    for t in added:
        where = f" in group {_q(t['group_name'])}" if t.get("group_name") else ""
        inst = f" — {_inst(t)}" if _inst(t) else ""
        fx = f" (+ {', '.join(t['effects'][:4])})" if t["effects"] and t["type"] != "MIDI" else ""
        ch["added"].append(f"+ {t['type']} {_q(t['name'])}{where}{inst}{fx}")
    for t in removed:
        inst = f" — {_inst(t)}" if _inst(t) else ""
        ch["removed"].append(f"− {t['type']} {_q(t['name'])}{inst}")

    old_main, new_main = old.get("main"), new.get("main")
    if old_main and new_main:
        pairs.append((old_main, new_main))

    clip_bits, mix_count = [], 0
    for o, n in pairs:
        name = _q(n["name"])
        if o.get("ident", o["name"]) != n.get("ident", n["name"]):
            ch["renamed"].append(f"{_q(o['name'])} → {name}")
        if o["instruments"] != n["instruments"]:
            ch["instruments"].append(f"{name}: {_inst(o) or 'none'} → {_inst(n) or 'none'}")
        oc, nc = Counter(o["effects"]), Counter(n["effects"])
        plus, minus = list((nc - oc).elements()), list((oc - nc).elements())
        if plus or minus:
            bits = [f"+ {d}" for d in plus] + [f"− {d}" for d in minus]
            ch["devices"].append(f"{name}: " + ", ".join(bits))
        if o.get("group") != n.get("group"):
            if n.get("group_name"):
                ch["groups"].append(f"{name} moved into group {_q(n['group_name'])}")
            else:
                ch["groups"].append(f"{name} moved out of group {_q(o['group_name'])}")
        if o["muted"] != n["muted"]:
            ch["state"].append(f"{name} {'muted' if n['muted'] else 'unmuted'}")
        if o["frozen"] != n["frozen"]:
            ch["state"].append(f"{name} {'frozen' if n['frozen'] else 'unfrozen'}")
        bits = []
        da = n["arr_clips"] - o["arr_clips"]
        ds = n["session_clips"] - o["session_clips"]
        dn = n["notes"] - o["notes"]
        if da:
            bits.append(f"{da:+d} arrangement clip{'s' if abs(da) != 1 else ''}")
        if ds:
            bits.append(f"{ds:+d} session clip{'s' if abs(ds) != 1 else ''}")
        if dn and abs(dn) >= 4:
            bits.append(f"{dn:+d} notes")
        if bits:
            clip_bits.append(f"{name}: " + ", ".join(bits))
        if abs(o["volume"] - n["volume"]) > 0.02 or abs(o["pan"] - n["pan"]) > 0.02:
            mix_count += 1
        new_samples = sorted(set(n["samples"]) - set(o["samples"]))
        if new_samples:
            shown = ", ".join(new_samples[:4]) + (f" (+{len(new_samples) - 4} more)" if len(new_samples) > 4 else "")
            ch["samples"].append(f"{name}: {shown}")
    ch["clips"] = clip_bits
    if mix_count:
        ch["state"].append(f"Volume/pan changed on {mix_count} track{'s' if mix_count != 1 else ''}")
    return ch


SECTIONS = [
    ("set", "Set"),
    ("added", "Tracks added"),
    ("removed", "Tracks removed"),
    ("renamed", "Tracks renamed"),
    ("instruments", "Instruments changed"),
    ("devices", "Devices added / removed"),
    ("groups", "Grouping"),
    ("samples", "New samples"),
    ("clips", "Clips & notes"),
    ("state", "Mix & track state"),
]


def format_lines(ch: dict) -> list:
    lines = []
    for key, title in SECTIONS:
        items = ch.get(key) or []
        if not items:
            continue
        lines.append(title)
        shown = items if key in ("added", "removed", "renamed", "instruments", "devices") else items[:12]
        lines += [f"  {x}" for x in shown]
        if len(items) > len(shown):
            lines.append(f"  … and {len(items) - len(shown)} more")
        lines.append("")
    if not lines:
        lines.append("No structural changes (only edits inside clips, automation or device settings).")
    return lines


def summary_line(ch: dict) -> str:
    bits = []
    if ch["added"]:
        names = [a.split('"')[1] for a in ch["added"] if '"' in a]
        if len(names) <= 2:
            bits.append("+ " + ", ".join(names))
        else:
            bits.append(f"+{len(names)} tracks")
    if ch["removed"]:
        bits.append(f"−{len(ch['removed'])} track{'s' if len(ch['removed']) != 1 else ''}")
    if ch["renamed"]:
        bits.append(f"{len(ch['renamed'])} renamed")
    if ch["instruments"]:
        bits.append("new instrument on " + ", ".join(x.split(":")[0].strip('"') for x in ch["instruments"][:2])
                    + ("…" if len(ch["instruments"]) > 2 else ""))
    if ch["devices"]:
        bits.append(f"devices on {len(ch['devices'])} track{'s' if len(ch['devices']) != 1 else ''}")
    for s in ch["set"]:
        if s.startswith(("Tempo", "Arrangement", "Key")):
            bits.append(s.replace("Arrangement length", "length"))
    if ch["samples"] and len(bits) < 4:
        bits.append("new samples")
    if not bits and ch["clips"]:
        bits.append(f"clip edits on {len(ch['clips'])} track{'s' if len(ch['clips']) != 1 else ''}")
    if not bits and ch["state"]:
        bits.append("mix tweaks")
    return " · ".join(bits) if bits else "minor edits"


def counts_line(snap: dict) -> str:
    c = Counter(t["type"] for t in snap["tracks"])
    order = ["MIDI", "Audio", "Group", "Return"]
    parts = [f"{c[k]} {k}" for k in order if c.get(k)]
    return f"{len(snap['tracks'])} tracks ({', '.join(parts)})"


def set_overview(snap: dict) -> str:
    bits = [snap.get("creator", "")]
    if snap.get("tempo"):
        bits.append(f"{snap['tempo']:g} BPM")
    if snap.get("key"):
        bits.append(snap["key"])
    bits.append(counts_line(snap))
    if snap.get("length_beats"):
        bits.append(f"{snap['length_beats'] / 4:g} bars")
    return " · ".join(b for b in bits if b)


def contents_lines(snap: dict) -> list:
    lines = []
    for t in snap["tracks"]:
        indent = "    " if t.get("group_name") else "  "
        inst = f" — {_inst(t)}" if _inst(t) else ""
        lines.append(f"{indent}{t['type']} {_q(t['name'])}{inst}")
    return lines
