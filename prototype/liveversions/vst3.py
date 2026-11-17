"""Replace VST2 / Audio Unit plug-ins in a Live set with their VST3 versions.

How settings survive
--------------------
* "VST2-compatible" VST3s (Valhalla, Soundtoys, Native Instruments, many JUCE
  plug-ins...) declare it through their class ID: "VST" + the VST2 four-char ID
  + the first 9 letters of the name. They accept VST2 data wrapped the way
  Cubase does it: a 'VstW' header followed by a 'CcnK'/'FBCh' bank holding the
  VST2 chunk. Live itself saves such plug-ins that way, so this is lossless.
* Other plug-ins with the same name: the old data can be passed as-is (works
  for JUCE plug-ins, whose VST2 chunk is their plain state), or the VST3 is
  added with default settings and the old plug-in is kept, switched off, next
  to it so the settings can be copied by hand.

Automation and macro/MIDI mappings point at a parameter's AutomationTarget Id,
which we keep. Only the parameter's ID inside the plug-in differs between VST2
(an index) and VST3 (a 32-bit ID), and those IDs can only be learned from a set
where Live has loaded the VST3. They are collected automatically from every set
this tool reads, or with `learn <folder>`.
"""
import copy
import glob
import json
import os
import plistlib
import re
import shutil
import sqlite3
import struct
import sys
import tempfile
from dataclasses import dataclass, field
from xml.etree import ElementTree as ET

from . import als, config

MODES = {
    "vst2state": "Keep settings (VST3 accepts the VST2 data)",
    "raw": "Keep settings: pass data as-is (try it)",
    "sidebyside": "Defaults + keep old plug-in (off) to copy settings",
    "default": "VST3 with default settings",
    "skip": "Don't convert",
}

PRESET_COMMON = ["OverwriteProtectionNumber", "MpeEnabled", "MpeSettings", "ParameterSettings",
                 "IsOn", "PowerMacroControlIndex", "PowerMacroMappingRange", "IsFolded",
                 "StoredAllParameters", "DeviceLomId", "DeviceViewLomId", "IsOnLomId",
                 "ParametersListWrapperLomId"]


# ----------------------------------------------------------------------------- installed VST3s

@dataclass
class Plugin:
    uid: bytes
    name: str
    vendor: str
    kind: str          # 'instr', 'audiofx', ...
    dev_id: str        # Live's device id, e.g. device:vst3:audiofx:5653...
    path: str = ""

    @property
    def is_instrument(self):
        return self.kind == "instr"


def live_plugin_dbs():
    if sys.platform == "win32":
        base = os.path.join(os.environ.get("LOCALAPPDATA", ""), "Ableton", "Live Database")
    else:
        base = os.path.expanduser("~/Library/Application Support/Ableton/Live Database")
    return sorted(glob.glob(os.path.join(base, "Live-plugins-*.db")), key=os.path.getmtime, reverse=True)


def load_catalog() -> list:
    """Installed VST3 plug-ins, read from (a copy of) Live's own plug-in database."""
    out, seen = [], set()
    for db in live_plugin_dbs()[:1]:
        tmp = tempfile.mkdtemp(prefix="liveversions-")
        try:
            for suffix in ("", "-wal", "-shm"):
                if os.path.exists(db + suffix):
                    shutil.copy2(db + suffix, os.path.join(tmp, "p.db" + suffix))
            con = sqlite3.connect(os.path.join(tmp, "p.db"))
            rows = con.execute(
                "select p.dev_identifier, p.name, p.vendor, coalesce(m.path,'') from plugins p "
                "left join plugin_modules m on p.module_id = m.module_id "
                "where p.dev_identifier like 'device:vst3:%'").fetchall()
            con.close()
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
        for dev_id, name, vendor, path in rows:
            m = re.match(r"device:vst3:(\w+):([0-9a-fA-F-]{36})", dev_id or "")
            if not m:
                continue
            uid = bytes.fromhex(m.group(2).replace("-", ""))
            if uid in seen:
                continue
            seen.add(uid)
            out.append(Plugin(uid, name or "", vendor or "", m.group(1), dev_id, path))
    return out


# ----------------------------------------------------------------------------- learned parameter IDs

def _learned_path():
    return os.path.join(config.app_dir(), "vst3_parameters.json")


_learned_cache = None


def learned() -> dict:
    global _learned_cache
    if _learned_cache is None:
        try:
            with open(_learned_path(), encoding="utf-8") as f:
                _learned_cache = json.load(f)
        except (OSError, ValueError):
            _learned_cache = {}
    return _learned_cache


def save_learned():
    if _learned_cache is not None:
        tmp = _learned_path() + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(_learned_cache, f)
        os.replace(tmp, _learned_path())


def _param_keys(params):
    """Stable keys for a plug-in's parameters; repeated names get #2, #3..."""
    seen = {}
    for p in params:
        name = als.val(p, "ParameterName", "")
        seen[name] = seen.get(name, 0) + 1
        yield p, name if seen[name] == 1 else f"{name}#{seen[name]}"


def uid_of(info) -> bytes:
    fields = [int(f.get("Value")) & 0xFFFFFFFF for f in info.find("Uid")]
    return struct.pack(">4I", *fields)


def learn_from_root(root) -> int:
    """Remember VST3 parameter IDs (and bus counts) from every VST3 in a set."""
    db, added = learned(), 0
    for pd in root.iter("PluginDevice"):
        info = pd.find("PluginDesc/Vst3PluginInfo")
        if info is None or info.find("Uid") is None:
            continue
        try:
            key = uid_of(info).hex()
        except (TypeError, ValueError, struct.error):
            continue
        entry = db.setdefault(key, {"name": als.val(info, "Name", ""), "params": {}})
        entry["inputs"] = als.val(info, "NumAudioInputs", entry.get("inputs"))
        entry["outputs"] = als.val(info, "NumAudioOutputs", entry.get("outputs"))
        plist = pd.find("ParameterList")
        for p, k in _param_keys(list(plist) if plist is not None else []):
            pid = als.val(p, "ParameterId")
            if pid is None or k in entry["params"]:
                continue
            entry["params"][k] = [pid, als.val(p, "ParameterIdFlankBool")]
            added += 1
    return added


def learn_paths(paths, progress=None) -> int:
    """Scan sets/folders (skipping cloud-only files) and store what was learned."""
    files = []
    for p in paths:
        if os.path.isfile(p) and p.lower().endswith(".als"):
            files.append(p)
        elif os.path.isdir(p):
            for dp, dn, fn in os.walk(p):
                dn[:] = [d for d in dn if d.lower() != "backup"]
                files += [os.path.join(dp, f) for f in fn if f.lower().endswith(".als")]
    total = 0
    for i, f in enumerate(files):
        if progress:
            progress(i, len(files), f)
        if als.is_cloud_only(f):
            continue
        try:
            total += learn_from_root(als.load_xml(f))
        except Exception:
            continue
    save_learned()
    return total


# ----------------------------------------------------------------------------- sources

@dataclass
class Source:
    fmt: str               # VST2 / AU
    name: str
    uid4: int | None       # VST2 unique ID (AU: subtype, which JUCE sets to the same code)
    is_instrument: bool
    chunk: bytes | None    # plug-in state
    chunk_kind: str        # 'bank' / 'juce' / 'au-data' / 'params' / ''
    version: int = 0
    programs: int = 0
    program: int = 0
    shell: bool = False


def _hex(text) -> bytes:
    h = "".join((text or "").split())
    return bytes.fromhex(h) if h else b""


def _int4(value):
    """Integer or four-char code (e.g. 'Ruum') -> int."""
    if value is None:
        return None
    v = value.strip()
    if re.fullmatch(r"-?\d+", v):
        return int(v) & 0xFFFFFFFF
    if len(v) == 4:
        return struct.unpack(">I", v.encode("latin-1"))[0]
    return None


def vst2_source(info) -> Source:
    vp = info.find("Preset/VstPreset")
    ptype = _int4(als.val(vp, "Type")) if vp is not None else None
    chunk = _hex(vp.find("Buffer").text) if vp is not None and vp.find("Buffer") is not None else b""
    kind = {0x46424368: "bank", 0x46504368: "bank"}.get(ptype, "params")  # 'FBCh' / 'FPCh'
    if kind == "bank" and chunk[:4] == b"VC2!":
        kind = "juce"
    return Source(
        fmt="VST2",
        name=als.val(info, "PlugName") or os.path.splitext(os.path.basename(als.val(info, "Path", "")))[0],
        uid4=_int4(als.val(info, "UniqueId")),
        is_instrument=als.val(info, "Category") == "2",
        chunk=chunk or None,
        chunk_kind=kind if chunk else "",
        version=int(als.val(vp, "PluginVersion", als.val(info, "Version", "0")) or 0),
        programs=int(als.val(vp, "ProgramCount", "1") or 1),
        program=int(als.val(vp, "ProgramNumber", "0") or 0),
        shell=als.val(info, "IsShellClient") == "true",
    )


def au_source(info) -> Source:
    """Audio Units: state is a property list; JUCE puts its state under 'jucePluginState'."""
    def find_val(*names):
        for e in info.iter():
            if e.tag in names and e.get("Value") is not None:
                return e.get("Value")
        return None

    subtype = _int4(find_val("ComponentSubType", "SubType"))
    ctype = find_val("ComponentType", "Type") or ""
    raw = b""
    for e in info.iter("Buffer"):
        raw = _hex(e.text)
        if raw:
            break
    chunk, kind = None, ""
    if raw:
        try:
            pl = plistlib.loads(raw)
            if isinstance(pl, dict) and isinstance(pl.get("jucePluginState"), bytes):
                chunk, kind = pl["jucePluginState"], "juce"
            elif isinstance(pl, dict) and isinstance(pl.get("data"), bytes):
                chunk, kind = pl["data"], "au-data"
        except Exception:
            chunk, kind = raw, "au-data"
    return Source(
        fmt="AU",
        name=find_val("Name", "PlugName") or "Audio Unit",
        uid4=subtype,
        is_instrument=_int4(ctype) == 0x61756D75,  # 'aumu'
        chunk=chunk,
        chunk_kind=kind,
    )


# ----------------------------------------------------------------------------- matching

def normalize(name: str) -> str:
    n = name.lower()
    n = re.sub(r"\((x64|x86|64[- ]?bit|stereo|mono|vst2?)\)", " ", n)
    n = re.sub(r"[\s_-]*(x64|x86_64|64[- ]?bit|vst2?|64)\s*$", "", n)
    n = re.sub(r"[\s_-]*v?\d+(\.\d+)+\s*$", "", n)  # "-1.5"
    return re.sub(r"[^a-z0-9]", "", n)


def compat_prefix(uid4: int) -> bytes:
    return b"VST" + struct.pack(">I", uid4 & 0xFFFFFFFF)


def find_target(src: Source, catalog: list):
    if src.uid4 is not None:
        pre = compat_prefix(src.uid4)
        hits = [p for p in catalog if p.uid[:7] == pre]
        if hits:
            tail = re.sub(r"[^a-z0-9 ]", "", src.name.lower()).encode()[:9]
            hits.sort(key=lambda p: (p.uid[7:].rstrip(b"\0") != tail.rstrip(b"\0"), len(p.name)))
            return hits[0], "compat"
    n = normalize(src.name)
    hits = [p for p in catalog if normalize(p.name) == n and n]
    hits.sort(key=lambda p: p.is_instrument != src.is_instrument)
    if hits:
        return hits[0], "name"
    return None, None


# ----------------------------------------------------------------------------- the plan

@dataclass
class Item:
    device: ET.Element
    track: str
    src: Source
    target: Plugin | None
    match: str | None
    mode: str
    note: str = ""
    automated: list = field(default_factory=list)   # names of automated/mapped params
    mapped: list = field(default_factory=list)      # those with a known VST3 ID

    @property
    def old_label(self):
        return f"{self.src.name} [{self.src.fmt}]"

    @property
    def new_label(self):
        return f"{self.target.name} [VST3]" if self.target else "— no VST3 found —"

    def modes(self):
        if not self.target:
            return ["skip"]
        m = []
        if self.src.chunk and self.src.chunk_kind in ("bank", "juce"):
            m.append("vst2state")
        if self.src.chunk:
            m.append("raw")
        return m + ["sidebyside", "default", "skip"]


@dataclass
class Plan:
    path: str
    root: ET.Element
    items: list
    catalog_size: int


def _envelope_targets(root) -> set:
    return {als.val(e, "PointeeId") for e in root.iter("EnvelopeTarget")}


def _param_ids(p):
    pv = p.find("ParameterValue")
    if pv is None:
        return set()
    return {e.get("Id") for e in pv if e.tag in ("AutomationTarget", "ModulationTarget") and e.get("Id")}


def _is_mapped(p, env_targets):
    pv = p.find("ParameterValue")
    has_midi = pv is not None and pv.find("KeyMidi") is not None
    return has_midi or bool(_param_ids(p) & env_targets)


def _default_mode(src: Source, match):
    if not match:
        return "skip", "No VST3 with this ID or name is installed."
    if src.shell:
        return "skip", "Shell plug-in (e.g. Waves): not supported."
    if match == "compat":
        if src.chunk_kind in ("bank", "juce"):
            return "vst2state", "VST3 declares it replaces this VST2 — settings carry over."
        if src.fmt == "AU" and src.chunk:
            return "raw", "VST3 replaces the VST2 with this ID; passing the AU state as-is."
        return "default", "Old plug-in stored no state chunk; VST3 starts from defaults."
    if src.chunk_kind == "juce":
        return "raw", "JUCE plug-in: its saved state is usually identical across formats."
    return "sidebyside", "Different plug-in IDs: settings can't be converted automatically."


def analyze(path: str, catalog=None) -> Plan:
    root = als.load_xml(path)
    # Learn from this set and its siblings first: they may already use the VST3s.
    learn_from_root(root)
    folder = os.path.dirname(os.path.abspath(path))
    sibs = sorted(glob.glob(os.path.join(folder, "*.als")), key=os.path.getmtime, reverse=True)
    for s in sibs[:6]:
        if os.path.abspath(s) != os.path.abspath(path) and not als.is_cloud_only(s):
            try:
                learn_from_root(als.load_xml(s))
            except Exception:
                pass
    save_learned()
    catalog = load_catalog() if catalog is None else catalog
    env = _envelope_targets(root)
    items = []
    ls = root.find("LiveSet")
    tracks = list(ls.find("Tracks"))
    for tag in ("MainTrack", "MasterTrack"):
        if ls.find(tag) is not None:
            tracks.append(ls.find(tag))
    for t in tracks:
        tname = als.val(t, "Name/EffectiveName") or t.tag.replace("Track", "")
        for dev in t.iter():
            if dev.tag not in ("PluginDevice", "AuPluginDevice"):
                continue
            desc = dev.find("PluginDesc")
            if desc is None or len(desc) == 0 or desc[0].tag not in ("VstPluginInfo", "AuPluginInfo"):
                continue
            info = desc[0]
            src = vst2_source(info) if info.tag == "VstPluginInfo" else au_source(info)
            target, match = find_target(src, catalog)
            mode, note = _default_mode(src, match)
            item = Item(dev, tname, src, target, match, mode, note)
            plist = dev.find("ParameterList")
            lmap = learned().get(target.uid.hex(), {}).get("params", {}) if target else {}
            for p, k in _param_keys(list(plist) if plist is not None else []):
                if _is_mapped(p, env):
                    item.automated.append(k)
                    if k in lmap:
                        item.mapped.append(k)
            items.append(item)
    return Plan(path, root, items, len(catalog))


# ----------------------------------------------------------------------------- building XML

def wrap_vst2_bank(chunk: bytes, fx_id: int, fx_version: int, programs: int, program: int) -> bytes:
    """VST2 chunk -> 'VstW' + 'CcnK'/'FBCh' bank (how Cubase/Live hand VST2 state to a VST3)."""
    head = struct.pack(">4sI4siIiii", b"CcnK", 0, b"FBCh", 2, fx_id & 0xFFFFFFFF,
                       fx_version, programs, program)
    bank = head + b"\0" * 124 + struct.pack(">i", len(chunk)) + chunk
    bank = bank[:4] + struct.pack(">i", len(bank) - 8) + bank[8:]
    return struct.pack(">4siii", b"VstW", 8, 1, 0) + bank


def _hex_text(data: bytes, depth: int) -> str:
    if not data:
        return None
    h = data.hex().upper()
    ind = "\t" * depth
    return "\n" + "\n".join(ind + h[i:i + 80] for i in range(0, len(h), 80)) + "\n" + "\t" * (depth - 1)


def _sub(parent, tag, value=None, **attrs):
    e = ET.SubElement(parent, tag, attrs)
    if value is not None:
        e.set("Value", str(value))
    return e


def _uid_elem(parent, uid: bytes):
    u = _sub(parent, "Uid")
    for i, v in enumerate(struct.unpack(">4i", uid)):
        _sub(u, f"Fields.{i}", v)
    return u


def _depth(root, elem, parents) -> int:
    d = 0
    while elem in parents:
        elem = parents[elem]
        d += 1
    return d


def build_vst3_info(old_info, target: Plugin, state: bytes, depth: int) -> ET.Element:
    lent = learned().get(target.uid.hex(), {})
    is_inst = target.is_instrument
    info = ET.Element("Vst3PluginInfo", {"Id": old_info.get("Id", "0")})
    _sub(info, "WinPosX", als.val(old_info, "WinPosX", "-1"))
    _sub(info, "WinPosY", als.val(old_info, "WinPosY", "-1"))
    _sub(info, "NumAudioInputs", lent.get("inputs") or ("0" if is_inst else "1"))
    _sub(info, "NumAudioOutputs", lent.get("outputs") or "1")
    _sub(info, "IsPlaceholderDevice", "false")
    preset = _sub(info, "Preset")
    old_preset = None
    for e in old_info.iter():
        if e.tag in ("VstPreset", "AuPreset"):
            old_preset = e
            break
    vp = _sub(preset, "Vst3Preset", Id=old_preset.get("Id", "0") if old_preset is not None else "0")
    for tag in PRESET_COMMON:
        src = old_preset.find(tag) if old_preset is not None else None
        if src is not None:
            vp.append(copy.deepcopy(src))
    _uid_elem(vp, target.uid)
    _sub(vp, "DeviceType", "1" if is_inst else "2")
    ps = _sub(vp, "ProcessorState")
    ps.text = _hex_text(state, depth + 4)
    _sub(vp, "ControllerState")
    _sub(vp, "Name", "")
    _sub(vp, "PresetRef")
    _sub(info, "Name", target.name)
    _uid_elem(info, target.uid)
    _sub(info, "DeviceType", "1" if is_inst else "2")
    ET.indent(info, space="\t", level=depth)  # keep the file readable like Live's own
    return info


def _state_for(item: Item) -> bytes:
    s = item.src
    if item.mode == "vst2state" and s.chunk:
        return wrap_vst2_bank(s.chunk, s.uid4 or 0, s.version, s.programs, s.program)
    if item.mode == "raw" and s.chunk:
        return s.chunk
    return b""


def _renumber_pointees(elem, next_id: int) -> int:
    for e in elem.iter():
        if "Id" in e.attrib and (e.tag in ("AutomationTarget", "ModulationTarget", "Pointee")
                                 or e.tag.endswith("ModulationTarget")):
            e.set("Id", str(next_id))
            next_id += 1
    return next_id


def convert(plan: Plan, out_path=None):
    """Apply the plan to a copy of the set. Returns (output path, report lines)."""
    from . import versions
    root = plan.root
    ls = root.find("LiveSet")
    parents = {c: p for p in root.iter() for c in p}
    env_targets = _envelope_targets(root)
    try:
        next_id = int(als.val(ls, "NextPointeeId", "0"))
    except ValueError:
        next_id = 0
    next_id = max(next_id, max((int(e.get("Id")) for e in root.iter("AutomationTarget")
                               if (e.get("Id") or "").isdigit()), default=0) + 1)
    dropped_targets = set()
    report = []
    done = 0

    for item in plan.items:
        if item.mode == "skip" or not item.target:
            report.append(f"  · {item.track}: {item.old_label} — kept ({item.note or 'skipped'})")
            continue
        dev = item.device
        desc = dev.find("PluginDesc")
        old_info = desc[0]

        if item.mode == "sidebyside":
            clone = copy.deepcopy(dev)
            next_id = _renumber_pointees(clone, next_id)
            on = clone.find("On/Manual")
            if on is not None:
                on.set("Value", "false")
            un = clone.find("UserName")
            if un is not None:
                un.set("Value", f"{item.src.name} (old {item.src.fmt}, off)")
            holder = parents[dev]
            sib_ids = [int(c.get("Id")) for c in holder if (c.get("Id") or "").isdigit()]
            clone.set("Id", str(max(sib_ids, default=0) + 1))
            holder.insert(list(holder).index(dev) + 1, clone)

        depth = _depth(root, desc, parents) + 1
        new_info = build_vst3_info(old_info, item.target, _state_for(item), depth)
        new_info.tail = old_info.tail
        desc.remove(old_info)
        desc.append(new_info)
        if dev.tag != "PluginDevice":
            dev.tag = "PluginDevice"

        # Point Live's browser link at the VST3.
        for bsc in dev.iter("BranchSourceContext"):
            bid = bsc.find("BranchDeviceId")
            if bid is not None:
                bid.set("Value", item.target.dev_id)
            bcp = bsc.find("BrowserContentPath")
            if bcp is not None:
                bcp.set("Value", f"query:Plugins#VST3:{item.target.vendor}:{item.target.name}")

        # Re-key parameters to VST3 IDs; drop the ones we can't map.
        lmap = learned().get(item.target.uid.hex(), {}).get("params", {})
        plist = dev.find("ParameterList")
        lost = []
        if plist is not None:
            for p, k in list(_param_keys(list(plist))):
                if k in lmap:
                    pid, flank = lmap[k]
                    pe = p.find("ParameterId")
                    pe.set("Value", str(pid))
                    if flank is not None and p.find("ParameterIdFlankBool") is None:
                        fe = ET.Element("ParameterIdFlankBool", {"Value": flank})
                        p.insert(list(p).index(pe) + 1, fe)
                else:
                    if _is_mapped(p, env_targets):
                        lost.append(k)
                    dropped_targets |= _param_ids(p)
                    plist.remove(p)

        what = {"vst2state": "settings kept", "raw": "settings passed as-is — check it",
                "sidebyside": "default settings; old plug-in kept next to it (off)",
                "default": "default settings"}[item.mode]
        line = f"  ✓ {item.track}: {item.old_label} → {item.new_label} — {what}"
        if item.automated:
            kept = len(item.automated) - len(lost)
            line += f"; automation/mappings kept for {kept}/{len(item.automated)} parameters"
        report.append(line)
        if lost:
            shown = ", ".join(lost[:6]) + ("…" if len(lost) > 6 else "")
            report.append(f"      lost automation for: {shown} (VST3 parameter IDs not learned yet)")
        done += 1

    # Remove envelopes that pointed at parameters we dropped.
    if dropped_targets:
        parents = {c: p for p in root.iter() for c in p}
        for et in list(root.iter("EnvelopeTarget")):
            if als.val(et, "PointeeId") in dropped_targets:
                env = parents.get(et)
                holder = parents.get(env)
                if env is not None and holder is not None:
                    holder.remove(env)

    nid = ls.find("NextPointeeId")
    if nid is not None:
        nid.set("Value", str(max(next_id, int(nid.get("Value", "0")))))

    out_path = out_path or versions.next_version_path(plan.path)
    als.save_xml(root, out_path)
    header = [f"Converted plug-ins to VST3 ({done} of {len(plan.items)}) from {os.path.basename(plan.path)}:"]
    return out_path, header + report
