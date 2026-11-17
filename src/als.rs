//! Reading/writing Ableton Live sets (gzip-compressed XML) and reducing a set to a
//! compact, diffable snapshot: tracks, instruments, effects, clip/note counts,
//! samples, tempo, key and locators.
use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::Path;

use crate::config;

pub const SNAPSHOT_SCHEMA: u32 = 6;

// ----------------------------------------------------------------------------- I/O

pub fn read_xml(path: &Path) -> std::io::Result<String> {
    let data = std::fs::read(path)?;
    if data.starts_with(&[0x1f, 0x8b]) {
        let mut s = String::with_capacity(data.len() * 8);
        flate2::read::GzDecoder::new(&data[..]).read_to_string(&mut s)?;
        Ok(s)
    } else {
        String::from_utf8(data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Write a set the way Live does (gzip, UTF-8). Goes through a temp file so a
/// crash never leaves a half-written set.
pub fn write_als(path: &Path, xml: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("als.tmp");
    {
        let f = std::fs::File::create(&tmp)?;
        let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::new(6));
        gz.write_all(xml.as_bytes())?;
        gz.finish()?.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn parse(xml: &str) -> Result<Document<'_>, String> {
    let opt = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: u32::MAX };
    Document::parse_with_options(xml, opt).map_err(|e| format!("Not a readable Live set: {e}"))
}

// ----------------------------------------------------------------------------- tree helpers

pub fn child<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == tag)
}

/// Follow a "A/B/C" path of child tags.
pub fn find<'a, 'i>(n: Node<'a, 'i>, path: &str) -> Option<Node<'a, 'i>> {
    let mut cur = n;
    for part in path.split('/') {
        cur = child(cur, part)?;
    }
    Some(cur)
}

/// The Value attribute at a child path, e.g. `val(track, "Name/EffectiveName")`.
pub fn val<'a>(n: Node<'a, '_>, path: &str) -> Option<&'a str> {
    find(n, path)?.attribute("Value")
}

pub fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}

pub fn tag<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

pub fn basename(p: &str) -> String {
    p.replace('\\', "/").rsplit('/').next().unwrap_or(p).to_string()
}

pub fn stem_of(p: &str) -> String {
    let b = basename(p);
    match b.rfind('.') {
        Some(i) if i > 0 => b[..i].to_string(),
        _ => b,
    }
}

// ----------------------------------------------------------------------------- devices

fn device_name(tag: &str) -> Option<&'static str> {
    Some(match tag {
        // instruments
        "OriginalSimpler" => "Simpler", "MultiSampler" => "Sampler", "Operator" => "Operator",
        "InstrumentVector" => "Wavetable", "UltraAnalog" => "Analog", "Collision" => "Collision",
        "LoungeLizard" => "Electric", "StringStudio" => "Tension", "InstrumentImpulse" => "Impulse",
        "Drift" => "Drift", "InstrumentMeld" | "Meld" => "Meld",
        "ExternalInstrument" => "External Instrument",
        // audio effects
        "Eq8" => "EQ Eight", "FilterEQ3" => "EQ Three", "ChannelEq" => "Channel EQ",
        "Compressor2" => "Compressor", "GlueCompressor" => "Glue Compressor",
        "MultibandDynamics" => "Multiband Dynamics", "Limiter" => "Limiter", "Gate" => "Gate",
        "StereoGain" => "Utility", "AutoFilter" | "AutoFilter2" => "Auto Filter",
        "AutoPan" => "Auto Pan", "AutoPan2" => "Auto Pan-Tremolo", "Saturator" => "Saturator",
        "Reverb" => "Reverb", "Hybrid" => "Hybrid Reverb", "Delay" => "Delay", "Echo" => "Echo",
        "PingPongDelay" => "Ping Pong Delay", "FilterDelay" => "Filter Delay",
        "GrainDelay" => "Grain Delay", "BeatRepeat" => "Beat Repeat", "Chorus2" => "Chorus-Ensemble",
        "Chorus" => "Chorus", "PhaserNew" => "Phaser-Flanger", "Phaser" => "Phaser",
        "Flanger" => "Flanger", "Redux2" | "Redux" => "Redux", "Vinyl" => "Vinyl Distortion",
        "Overdrive" => "Overdrive", "Erosion" => "Erosion", "Tuner" => "Tuner",
        "SpectrumAnalyzer" => "Spectrum", "Amp" => "Amp", "Cabinet" => "Cabinet", "Corpus" => "Corpus",
        "Resonator" => "Resonators", "FrequencyShifter" => "Frequency Shifter",
        "Vocoder" => "Vocoder", "DrumBuss" => "Drum Buss", "Pedal" => "Pedal", "Looper" => "Looper",
        "SpectralResonator" => "Spectral Resonator", "SpectralTime" => "Spectral Time",
        "Roar" => "Roar", "Shifter" => "Shifter", "ExternalAudioEffect" => "External Audio Effect",
        "Transmute" => "Transmute",
        // MIDI effects
        "MidiArpeggiator" => "Arpeggiator", "MidiChord" => "Chord", "MidiNoteLength" => "Note Length",
        "MidiPitcher" => "Pitch", "MidiRandom" => "Random", "MidiScale" => "Scale",
        "MidiVelocity" => "Velocity",
        _ => return None,
    })
}

const INSTRUMENT_TAGS: &[&str] = &[
    "OriginalSimpler", "MultiSampler", "Operator", "InstrumentVector", "UltraAnalog", "Collision",
    "LoungeLizard", "StringStudio", "InstrumentImpulse", "Drift", "InstrumentMeld", "Meld",
    "ExternalInstrument", "MxDeviceInstrument",
];

fn rack_kind(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "InstrumentGroupDevice" => "Instrument Rack",
        "AudioEffectGroupDevice" => "Audio Effect Rack",
        "MidiEffectGroupDevice" => "MIDI Effect Rack",
        "DrumGroupDevice" => "Drum Rack",
        _ => return None,
    })
}

fn split_camel(tag: &str) -> String {
    let t = tag.strip_prefix("Midi").or_else(|| tag.strip_prefix("Mx")).unwrap_or(tag);
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in t.chars() {
        if ch.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        out.push(ch);
    }
    if out.is_empty() { tag.to_string() } else { out }
}

pub struct PluginInfo<'a, 'i> {
    pub format: String,
    pub name: String,
    pub is_instrument: bool,
    _node: std::marker::PhantomData<Node<'a, 'i>>,
}

pub fn plugin_info<'a, 'i>(dev: Node<'a, 'i>) -> PluginInfo<'a, 'i> {
    let info = child(dev, "PluginDesc").and_then(|d| elements(d).next());
    let Some(info) = info else {
        return PluginInfo { format: "Plug-in".into(), name: "Plug-in".into(), is_instrument: false, _node: std::marker::PhantomData, };
    };
    match tag(info) {
        "Vst3PluginInfo" => PluginInfo {
            format: "VST3".into(),
            name: val(info, "Name").unwrap_or("VST3").into(),
            is_instrument: val(info, "DeviceType") == Some("1"),
            _node: std::marker::PhantomData,
        },
        "VstPluginInfo" => {
            let name = val(info, "PlugName")
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| stem_of(val(info, "Path").unwrap_or("VST")));
            PluginInfo { format: "VST2".into(), name, is_instrument: val(info, "Category") == Some("2"), _node: std::marker::PhantomData, }
        }
        "AuPluginInfo" => {
            let name = val(info, "Name").or_else(|| val(info, "PlugName")).unwrap_or("Audio Unit");
            let ctype = val(info, "ComponentType").unwrap_or("").trim();
            PluginInfo {
                format: "AU".into(),
                name: name.into(),
                is_instrument: ctype == "aumu" || ctype == "1635085685",
                _node: std::marker::PhantomData,
            }
        }
        other => PluginInfo {
            format: other.replace("PluginInfo", ""),
            name: val(info, "Name").unwrap_or("Plug-in").into(),
            is_instrument: false,
            _node: std::marker::PhantomData,
        },
    }
}

fn m4l_name(dev: Node) -> String {
    for e in dev.descendants() {
        if let Some(v) = e.attribute("Value") {
            if v.to_lowercase().ends_with(".amxd") {
                return stem_of(v);
            }
        }
    }
    "Max for Live device".into()
}

pub fn device_label(dev: Node) -> String {
    let t = tag(dev);
    if t == "PluginDevice" || t == "AuPluginDevice" {
        let p = plugin_info(dev);
        return format!("{} [{}]", p.name, p.format);
    }
    if t.starts_with("MxDevice") {
        return format!("{} [M4L]", m4l_name(dev));
    }
    device_name(t).map(String::from).unwrap_or_else(|| split_camel(t))
}

fn is_instrument(dev: Node) -> bool {
    let t = tag(dev);
    if t == "PluginDevice" || t == "AuPluginDevice" {
        return plugin_info(dev).is_instrument;
    }
    INSTRUMENT_TAGS.contains(&t)
}

/// The Devices list of one rack chain (the first, i.e. shallowest, one).
fn branch_devices<'a, 'i>(branch: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    branch
        .descendants()
        .find(|d| d.is_element() && tag(*d) == "Devices")
        .map(|d| elements(d).collect())
        .unwrap_or_default()
}

/// Flatten a device chain. A rack loaded from a preset (it has a name) counts as
/// one entry, so changing its insides isn't reported device by device; the
/// instruments inside racks are always found so we can say what plays on the track.
fn walk_chain(devices: &[Node], instruments: &mut Vec<String>, effects: Option<&mut Vec<String>>, rack: &str) {
    let mut sink = Vec::new();
    let effects = match effects {
        Some(e) => e,
        None => &mut sink,
    };
    for &dev in devices {
        let t = tag(dev);
        let user = val(dev, "UserName").unwrap_or("").trim().to_string();
        if let Some(kind) = rack_kind(t) {
            if t == "DrumGroupDevice" {
                instruments.push(if user.is_empty() { kind.into() } else { format!("{kind} '{user}'") });
                continue;
            }
            let inner: Vec<Node> = child(dev, "Branches").map(|b| elements(b).collect()).unwrap_or_default();
            if t == "InstrumentGroupDevice" {
                let mut found = Vec::new();
                let ctx = if user.is_empty() { rack } else { user.as_str() };
                for b in &inner {
                    let devs = branch_devices(*b);
                    if user.is_empty() {
                        walk_chain(&devs, &mut found, Some(effects), ctx);
                    } else {
                        walk_chain(&devs, &mut found, None, ctx);
                    }
                }
                let none = found.is_empty();
                instruments.extend(found);
                if !user.is_empty() && none {
                    instruments.push(format!("{kind} '{user}'"));
                }
                continue;
            }
            if !user.is_empty() {
                effects.push(format!("{kind} '{user}'"));
            } else {
                for b in &inner {
                    walk_chain(&branch_devices(*b), instruments, Some(effects), rack);
                }
            }
            continue;
        }
        let label = device_label(dev);
        if is_instrument(dev) {
            instruments.push(if rack.is_empty() { label } else { format!("{label} (in '{rack}')") });
        } else {
            effects.push(label);
        }
    }
}

// ----------------------------------------------------------------------------- snapshot

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TrackSnap {
    pub id: String,
    pub ttype: String,
    pub name: String,
    /// Name without Live's "#" auto-number / leading track numbers, for rename detection.
    pub ident: String,
    pub group: String,
    pub group_name: String,
    pub muted: bool,
    pub frozen: bool,
    pub instruments: Vec<String>,
    pub effects: Vec<String>,
    pub session_clips: usize,
    pub arr_clips: usize,
    pub arr_end: f64,
    pub notes: usize,
    pub samples: Vec<String>,
    pub volume: f64,
    pub pan: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Snapshot {
    pub schema: u32,
    pub creator: String,
    pub tempo: Option<f64>,
    pub key: String,
    pub scenes: usize,
    pub locators: Vec<String>,
    pub length_beats: f64,
    pub tracks: Vec<TrackSnap>,
    pub main: Option<TrackSnap>,
}

fn is_clip(n: &Node) -> bool {
    n.is_element() && matches!(tag(*n), "MidiClip" | "AudioClip")
}

fn sample_names(t: Node) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for r in t.descendants().filter(|n| n.is_element() && tag(*n) == "SampleRef") {
        let Some(fr) = child(r, "FileRef") else { continue };
        let mut p = val(fr, "Path").filter(|s| !s.is_empty()).or_else(|| val(fr, "Name")).unwrap_or("").to_string();
        if p.is_empty() {
            let rel: Vec<&str> = fr
                .descendants()
                .filter(|e| e.is_element() && tag(*e) == "RelativePathElement")
                .filter_map(|e| e.attribute("Dir"))
                .collect();
            p = rel.join("/");
        }
        if !p.is_empty() {
            names.insert(basename(&p));
        }
    }
    names.into_iter().collect()
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn track_snapshot(t: Node, ttype: &str) -> TrackSnap {
    let mut instruments = Vec::new();
    let mut effects = Vec::new();
    if let Some(devs) = find(t, "DeviceChain/DeviceChain/Devices") {
        let list: Vec<Node> = elements(devs).collect();
        walk_chain(&list, &mut instruments, Some(&mut effects), "");
    }
    let (mut session, mut arr) = (Vec::new(), Vec::new());
    if let Some(seq) = find(t, "DeviceChain/MainSequencer") {
        let in_slots: HashSet<roxmltree::NodeId> = child(seq, "ClipSlotList")
            .map(|s| s.descendants().filter(is_clip).map(|n| n.id()).collect())
            .unwrap_or_default();
        for c in seq.descendants().filter(is_clip) {
            if in_slots.contains(&c.id()) { session.push(c) } else { arr.push(c) }
        }
    }
    let notes = session
        .iter()
        .chain(arr.iter())
        .map(|c| c.descendants().filter(|n| n.is_element() && tag(*n) == "MidiNoteEvent").count())
        .sum();
    let arr_end = arr
        .iter()
        .filter_map(|c| val(*c, "CurrentEnd")?.parse::<f64>().ok())
        .fold(0.0, f64::max);
    let mixer = find(t, "DeviceChain/Mixer");
    let num = |p: &str| mixer.and_then(|m| val(m, p)).and_then(|v| v.parse::<f64>().ok()).map(round3).unwrap_or(0.0);
    let effective = val(t, "Name/EffectiveName").filter(|s| !s.is_empty()).or_else(|| val(t, "Name/UserName")).unwrap_or("");
    let user = val(t, "Name/UserName").unwrap_or("");
    let re = regex::Regex::new(r"^\s*(#|\d+)\s*[-_.]?\s*").unwrap();
    let ident = re.replace(if user.is_empty() { effective } else { user }, "").trim().to_lowercase();
    TrackSnap {
        id: t.attribute("Id").unwrap_or("").into(),
        ttype: ttype.into(),
        name: effective.into(),
        ident,
        group: val(t, "TrackGroupId").unwrap_or("-1").into(),
        group_name: String::new(),
        muted: mixer.and_then(|m| val(m, "Speaker/Manual")) == Some("false"),
        frozen: val(t, "Freeze") == Some("true"),
        instruments,
        effects,
        session_clips: session.len(),
        arr_clips: arr.len(),
        arr_end,
        notes,
        samples: sample_names(t),
        volume: num("Volume/Manual"),
        pan: num("Pan/Manual"),
    }
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn snapshot_from_doc(doc: &Document) -> Snapshot {
    let root = doc.root_element();
    let Some(ls) = child(root, "LiveSet") else { return Snapshot::default() };
    let mut tracks: Vec<TrackSnap> = child(ls, "Tracks")
        .map(|ts| {
            elements(ts)
                .map(|t| {
                    let ty = match tag(t) {
                        "MidiTrack" => "MIDI",
                        "AudioTrack" => "Audio",
                        "GroupTrack" => "Group",
                        "ReturnTrack" => "Return",
                        o => o,
                    };
                    track_snapshot(t, ty)
                })
                .collect()
        })
        .unwrap_or_default();
    let names: std::collections::HashMap<String, String> = tracks.iter().map(|t| (t.id.clone(), t.name.clone())).collect();
    for t in tracks.iter_mut() {
        if t.group != "-1" {
            t.group_name = names.get(&t.group).cloned().unwrap_or_default();
        }
    }
    let main = child(ls, "MainTrack").or_else(|| child(ls, "MasterTrack"));
    let main_snap = main.map(|m| {
        let mut s = track_snapshot(m, "Main");
        s.name = "Main".into();
        s.ident = "main".into();
        s
    });
    let tempo = main
        .and_then(|m| val(m, "DeviceChain/Mixer/Tempo/Manual"))
        .and_then(|v| v.parse::<f64>().ok())
        .map(|t| (t * 100.0).round() / 100.0);
    let mut key = String::new();
    if val(ls, "InKey") == Some("true") {
        if let Some(si) = child(ls, "ScaleInformation") {
            let r = val(si, "Root").or_else(|| val(si, "RootNote")).unwrap_or("0");
            if let Ok(r) = r.parse::<i64>() {
                let scale = val(si, "Name").unwrap_or("");
                // Live 11 stores the scale's name, Live 12 an index into its scale list.
                let scale = if !scale.is_empty() && scale.chars().all(|c| c.is_ascii_digit()) {
                    format!("(scale #{scale})")
                } else {
                    scale.to_string()
                };
                key = format!("{} {}", NOTE_NAMES[r.rem_euclid(12) as usize], scale).trim().to_string();
            }
        }
    }
    let locators = find(ls, "Locators/Locators")
        .map(|l| elements(l).map(|x| val(x, "Name").unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let length_beats = tracks.iter().map(|t| t.arr_end).fold(0.0, f64::max);
    Snapshot {
        schema: SNAPSHOT_SCHEMA,
        creator: root.attribute("Creator").unwrap_or("").into(),
        tempo,
        key,
        scenes: child(ls, "Scenes").map(|s| elements(s).count()).unwrap_or(0),
        locators,
        length_beats,
        tracks,
        main: main_snap,
    }
}

// ----------------------------------------------------------------------------- cache

fn cache_file(path: &Path) -> Option<std::path::PathBuf> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key = format!("{}|{}|{}|{}", abs.to_string_lossy().to_lowercase(), md.len(), mtime, SNAPSHOT_SCHEMA);
    Some(config::cache_dir().join(format!("{}.json", sha1_smol::Sha1::from(key).digest())))
}

/// Snapshot of a set, cached by path+size+mtime so reopening the browser is instant.
/// Reading a set also teaches the VST3 converter the parameter IDs of its VST3s.
pub fn snapshot(path: &Path) -> Result<Snapshot, String> {
    let cf = cache_file(path);
    if let Some(cf) = &cf {
        if let Ok(s) = std::fs::read_to_string(cf) {
            if let Ok(snap) = serde_json::from_str::<Snapshot>(&s) {
                return Ok(snap);
            }
        }
    }
    let xml = read_xml(path).map_err(|e| format!("Can't read {}: {e}", path.display()))?;
    let doc = parse(&xml)?;
    let snap = snapshot_from_doc(&doc);
    if crate::vst3::learn_from_doc(&doc) > 0 {
        crate::vst3::save_learned();
    }
    if let Some(cf) = cf {
        if let Ok(s) = serde_json::to_string(&snap) {
            let _ = std::fs::write(cf, s);
        }
    }
    Ok(snap)
}

/// True for OneDrive/iCloud placeholders that would be downloaded if read.
pub fn is_cloud_only(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(md) = std::fs::metadata(path) {
            return md.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0;
        }
    }
    let _ = path;
    false
}
