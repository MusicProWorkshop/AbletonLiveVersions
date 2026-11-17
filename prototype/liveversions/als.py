"""Read/write Ableton Live sets and reduce them to a compact, diffable snapshot.

An .als file is gzip-compressed XML. The snapshot keeps only what matters for a
human-readable change log: tracks, their instruments and effects, clip/note
counts, samples, tempo, key and locators.
"""
import gzip
import hashlib
import json
import os
import re
from xml.etree import ElementTree as ET

from . import config

SNAPSHOT_SCHEMA = 5

TRACK_TYPES = {
    "MidiTrack": "MIDI",
    "AudioTrack": "Audio",
    "GroupTrack": "Group",
    "ReturnTrack": "Return",
}

RACKS = {
    "InstrumentGroupDevice": "Instrument Rack",
    "AudioEffectGroupDevice": "Audio Effect Rack",
    "MidiEffectGroupDevice": "MIDI Effect Rack",
    "DrumGroupDevice": "Drum Rack",
}

# Live's XML tag -> name shown in Live. Unknown tags fall back to a split CamelCase.
DEVICE_NAMES = {
    # instruments
    "OriginalSimpler": "Simpler", "MultiSampler": "Sampler", "Operator": "Operator",
    "InstrumentVector": "Wavetable", "UltraAnalog": "Analog", "Collision": "Collision",
    "LoungeLizard": "Electric", "StringStudio": "Tension", "InstrumentImpulse": "Impulse",
    "Drift": "Drift", "InstrumentMeld": "Meld", "Meld": "Meld",
    "ExternalInstrument": "External Instrument",
    # audio effects
    "Eq8": "EQ Eight", "FilterEQ3": "EQ Three", "ChannelEq": "Channel EQ",
    "Compressor2": "Compressor", "GlueCompressor": "Glue Compressor",
    "MultibandDynamics": "Multiband Dynamics", "Limiter": "Limiter", "Gate": "Gate",
    "StereoGain": "Utility", "AutoFilter": "Auto Filter", "AutoFilter2": "Auto Filter",
    "AutoPan": "Auto Pan", "AutoPan2": "Auto Pan-Tremolo", "Saturator": "Saturator",
    "Reverb": "Reverb", "Hybrid": "Hybrid Reverb", "Delay": "Delay", "Echo": "Echo",
    "PingPongDelay": "Ping Pong Delay", "FilterDelay": "Filter Delay",
    "GrainDelay": "Grain Delay", "BeatRepeat": "Beat Repeat", "Chorus2": "Chorus-Ensemble",
    "Chorus": "Chorus", "PhaserNew": "Phaser-Flanger", "Phaser": "Phaser",
    "Flanger": "Flanger", "Redux2": "Redux", "Redux": "Redux", "Vinyl": "Vinyl Distortion",
    "Overdrive": "Overdrive", "Erosion": "Erosion", "Tuner": "Tuner",
    "SpectrumAnalyzer": "Spectrum", "Amp": "Amp", "Cabinet": "Cabinet", "Corpus": "Corpus",
    "Resonator": "Resonators", "FrequencyShifter": "Frequency Shifter",
    "Vocoder": "Vocoder", "DrumBuss": "Drum Buss", "Pedal": "Pedal", "Looper": "Looper",
    "SpectralResonator": "Spectral Resonator", "SpectralTime": "Spectral Time",
    "Roar": "Roar", "Shifter": "Shifter", "ExternalAudioEffect": "External Audio Effect",
    "Transmute": "Transmute", "Dimension": "Dimension",
    # MIDI effects
    "MidiArpeggiator": "Arpeggiator", "MidiChord": "Chord", "MidiNoteLength": "Note Length",
    "MidiPitcher": "Pitch", "MidiRandom": "Random", "MidiScale": "Scale",
    "MidiVelocity": "Velocity",
}

INSTRUMENT_TAGS = {
    "OriginalSimpler", "MultiSampler", "Operator", "InstrumentVector", "UltraAnalog",
    "Collision", "LoungeLizard", "StringStudio", "InstrumentImpulse", "Drift",
    "InstrumentMeld", "Meld", "ExternalInstrument", "MxDeviceInstrument",
}

NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


# ----------------------------------------------------------------------------- I/O

def load_xml(path: str) -> ET.Element:
    with open(path, "rb") as f:
        data = f.read()
    if data[:2] == b"\x1f\x8b":
        data = gzip.decompress(data)
    return ET.fromstring(data)


def save_xml(root: ET.Element, path: str) -> None:
    """Write a set the way Live does (gzip, UTF-8, double-quoted declaration)."""
    body = ET.tostring(root, encoding="unicode")
    data = ('<?xml version="1.0" encoding="UTF-8"?>\n' + body + "\n").encode("utf-8")
    tmp = path + ".tmp"
    with open(tmp, "wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw,
                                              compresslevel=6, mtime=0) as gz:
        gz.write(data)
    os.replace(tmp, path)


def val(elem, path, default=None):
    """Value attribute of a child element, e.g. val(track, 'Name/EffectiveName')."""
    if elem is None:
        return default
    e = elem.find(path)
    if e is None:
        return default
    return e.get("Value", default)


# ----------------------------------------------------------------------------- devices

def _split_camel(tag: str) -> str:
    tag = re.sub(r"^(Midi|Mx)", "", tag)
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", tag) or tag


def plugin_info(dev: ET.Element):
    """(format, name, is_instrument, info_element) for a PluginDevice/AuPluginDevice."""
    desc = dev.find("PluginDesc")
    if desc is None or len(desc) == 0:
        return None, "Plug-in", False, None
    info = desc[0]
    if info.tag == "Vst3PluginInfo":
        return "VST3", val(info, "Name", "VST3"), val(info, "DeviceType") == "1", info
    if info.tag == "VstPluginInfo":
        name = val(info, "PlugName") or os.path.splitext(os.path.basename(val(info, "Path", "VST")))[0]
        return "VST2", name, val(info, "Category") == "2", info
    if info.tag == "AuPluginInfo":
        name = val(info, "Name") or val(info, "PlugName") or "Audio Unit"
        ctype = (val(info, "ComponentType") or "").strip()
        is_inst = ctype in ("aumu", "1635085685")  # 'aumu' as text or as int
        return "AU", name, is_inst, info
    return info.tag.replace("PluginInfo", ""), val(info, "Name", "Plug-in"), False, info


def _m4l_name(dev: ET.Element) -> str:
    for e in dev.iter():
        v = e.get("Value") or ""
        if v.lower().endswith(".amxd"):
            return os.path.splitext(os.path.basename(v.replace("\\", "/")))[0]
    return "Max for Live device"


def device_label(dev: ET.Element) -> str:
    """Human name of a single (non-rack) device, e.g. 'Serum 2 [VST3]' or 'Saturator'."""
    tag = dev.tag
    if tag in ("PluginDevice", "AuPluginDevice"):
        fmt, name, _, _ = plugin_info(dev)
        return f"{name} [{fmt}]"
    if tag.startswith("MxDevice"):
        return f"{_m4l_name(dev)} [M4L]"
    return DEVICE_NAMES.get(tag) or _split_camel(tag)


def is_instrument(dev: ET.Element) -> bool:
    tag = dev.tag
    if tag in ("PluginDevice", "AuPluginDevice"):
        return plugin_info(dev)[2]
    return tag in INSTRUMENT_TAGS


def branch_devices(branch: ET.Element):
    """The Devices list of one rack chain (the first, i.e. shallowest, one)."""
    for e in branch.iter("Devices"):
        return list(e)
    return []


def walk_chain(devices, instruments: list, effects: list, rack: str = ""):
    """Flatten a device chain.

    Named racks (loaded from a preset) count as one entry so a rack preset with
    ten devices inside shows up as one change, not ten. Instruments inside
    racks are always found so we can say what plays on the track.
    """
    for dev in devices:
        tag = dev.tag
        user = (val(dev, "UserName") or "").strip()
        if tag in RACKS:
            kind = RACKS[tag]
            if tag == "DrumGroupDevice":
                instruments.append(f"{kind} '{user}'" if user else kind)
                continue
            branches = dev.find("Branches")
            inner = list(branches) if branches is not None else []
            if tag == "InstrumentGroupDevice":
                # A named rack is a preset: report its instrument(s) "in 'Preset'"
                # but not every effect hidden inside it.
                found = []
                inner_fx = [] if user else effects
                for b in inner:
                    walk_chain(branch_devices(b), found, inner_fx, user or rack)
                instruments.extend(found)
                if user and not found:
                    instruments.append(f"{kind} '{user}'")
                continue
            if user:
                effects.append(f"{kind} '{user}'")
            else:
                for b in inner:
                    walk_chain(branch_devices(b), instruments, effects, rack)
            continue
        label = device_label(dev)
        if is_instrument(dev):
            instruments.append(f"{label} (in '{rack}')" if rack else label)
        else:
            effects.append(label)


# ----------------------------------------------------------------------------- tracks

def _clips(track: ET.Element):
    seq = track.find("DeviceChain/MainSequencer")
    if seq is None:
        return [], []
    allc = [c for c in seq.iter() if c.tag in ("MidiClip", "AudioClip")]
    slots = seq.find("ClipSlotList")
    session = [c for c in slots.iter() if c.tag in ("MidiClip", "AudioClip")] if slots is not None else []
    sid = {id(c) for c in session}
    return session, [c for c in allc if id(c) not in sid]


def _sample_names(elem: ET.Element) -> set:
    names = set()
    for ref in elem.iter("SampleRef"):
        fr = ref.find("FileRef")
        if fr is None:
            continue
        p = val(fr, "Path") or val(fr, "Name")
        if not p:
            rel = [e.get("Dir") for e in fr.iter("RelativePathElement") if e.get("Dir")]
            p = val(fr, "Name") or "/".join(rel)
        if p:
            names.add(os.path.basename(p.replace("\\", "/")))
    return names


def _track_snapshot(t: ET.Element, ttype: str) -> dict:
    instruments, effects = [], []
    devs = t.find("DeviceChain/DeviceChain/Devices")
    if devs is not None:
        walk_chain(list(devs), instruments, effects)
    session, arr = _clips(t)
    notes = sum(1 for c in session + arr for _ in c.iter("MidiNoteEvent"))
    arr_end = 0.0
    for c in arr:
        try:
            arr_end = max(arr_end, float(val(c, "CurrentEnd", "0")))
        except ValueError:
            pass
    mixer = t.find("DeviceChain/Mixer")

    def num(path):
        try:
            return round(float(val(mixer, path, "0")), 3)
        except ValueError:
            return 0.0

    effective = val(t, "Name/EffectiveName") or val(t, "Name/UserName") or ""
    user = val(t, "Name/UserName") or ""
    # Identity for rename detection: Live's "#" auto-number token and leading
    # track numbers shift whenever tracks move, so they don't count as renames.
    ident = re.sub(r"^\s*(#|\d+)\s*[-_.]?\s*", "", user or effective).strip().lower()
    return {
        "id": t.get("Id"),
        "type": ttype,
        "name": effective,
        "ident": ident,
        "group": val(t, "TrackGroupId", "-1"),
        "muted": val(mixer, "Speaker/Manual", "true") == "false",
        "frozen": val(t, "Freeze", "false") == "true",
        "instruments": instruments,
        "effects": effects,
        "session_clips": len(session),
        "arr_clips": len(arr),
        "arr_end": arr_end,
        "notes": notes,
        "samples": sorted(_sample_names(t)),
        "volume": num("Volume/Manual"),
        "pan": num("Pan/Manual"),
    }


def snapshot_from_xml(root: ET.Element) -> dict:
    ls = root.find("LiveSet")
    tracks = []
    for t in ls.find("Tracks"):
        tracks.append(_track_snapshot(t, TRACK_TYPES.get(t.tag, t.tag)))
    names = {t["id"]: t["name"] for t in tracks}
    for t in tracks:
        t["group_name"] = names.get(t["group"], "") if t["group"] != "-1" else ""
    main = ls.find("MainTrack")
    if main is None:
        main = ls.find("MasterTrack")
    main_snap = _track_snapshot(main, "Main") if main is not None else None
    if main_snap:
        main_snap["name"] = "Main"
    tempo = None
    if main is not None:
        try:
            tempo = round(float(val(main, "DeviceChain/Mixer/Tempo/Manual")), 2)
        except (TypeError, ValueError):
            pass
    key = ""
    if val(ls, "InKey") == "true" and ls.find("ScaleInformation") is not None:
        try:
            r = val(ls, "ScaleInformation/Root") or val(ls, "ScaleInformation/RootNote") or "0"
            scale = val(ls, "ScaleInformation/Name", "")
            # Live 11 stores the scale's name, Live 12 an index into its scale list.
            scale = f"(scale #{scale})" if scale.isdigit() else scale
            key = f"{NOTE_NAMES[int(r) % 12]} {scale}".strip()
        except ValueError:
            pass
    locators = [val(l, "Name", "") for l in ls.iterfind("Locators/Locators/Locator")]
    scenes = ls.find("Scenes")
    return {
        "schema": SNAPSHOT_SCHEMA,
        "creator": root.get("Creator", ""),
        "tempo": tempo,
        "key": key,
        "scenes": len(scenes) if scenes is not None else 0,
        "locators": locators,
        "length_beats": max([t["arr_end"] for t in tracks] + [0.0]),
        "tracks": tracks,
        "main": main_snap,
    }


# ----------------------------------------------------------------------------- cache

def _cache_file(path: str) -> str:
    st = os.stat(path)
    key = f"{os.path.abspath(path).lower()}|{st.st_size}|{st.st_mtime_ns}|{SNAPSHOT_SCHEMA}"
    return os.path.join(config.cache_dir(), hashlib.sha1(key.encode()).hexdigest() + ".json")


def snapshot(path: str) -> dict:
    """Snapshot of a set, cached by path+size+mtime so re-opening the browser is fast."""
    cf = _cache_file(path)
    try:
        with open(cf, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        pass
    root = load_xml(path)
    snap = snapshot_from_xml(root)
    try:  # free side effect: remember VST3 parameter IDs for the VST3 converter
        from . import vst3
        if vst3.learn_from_root(root):
            vst3.save_learned()
    except Exception:
        pass
    try:
        with open(cf, "w", encoding="utf-8") as f:
            json.dump(snap, f)
    except OSError:
        pass
    return snap


def is_cloud_only(path: str) -> bool:
    """True for OneDrive/iCloud placeholders that would be downloaded on read."""
    try:
        attrs = getattr(os.stat(path), "st_file_attributes", 0)
    except OSError:
        return False
    return bool(attrs & (0x1000 | 0x40000 | 0x400000))
