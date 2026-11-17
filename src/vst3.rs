//! Replace VST2 / Audio Unit plug-ins in a Live set with their VST3 versions.
//!
//! How settings survive:
//! * "VST2-compatible" VST3s (Valhalla, Soundtoys, Native Instruments, many JUCE
//!   plug-ins…) declare it through their class ID: "VST" + the VST2 four-char ID
//!   + the first 9 letters of the name. They accept VST2 data wrapped the way
//!   Cubase does it: a 'VstW' header + a 'CcnK'/'FBCh' bank holding the VST2 chunk.
//!   Live saves such plug-ins exactly like that, so this is lossless.
//! * Other plug-ins with the same name: pass the old data as-is (works for JUCE
//!   plug-ins), or add the VST3 with default settings and keep the old plug-in,
//!   switched off, next to it so settings can be copied by hand.
//!
//! Automation and macro/MIDI mappings point at a parameter's AutomationTarget Id,
//! which is kept. Only the parameter's ID inside the plug-in differs between VST2
//! (an index) and VST3; those IDs are learned from sets where Live loaded the VST3.
//!
//! The set is edited as text: only the changed byte ranges are replaced, so the
//! rest of the file stays exactly as Live wrote it.
use roxmltree::Node;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::als::{self, child, elements, find, tag, val};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Vst2State,
    Raw,
    /// Effects: VST3 with defaults, the old plug-in kept next to it, switched off.
    SideBySide,
    /// Instruments: the track is duplicated; the copy gets the VST3 (defaults) and
    /// plays, the original keeps the old instrument and is muted. (A chain can't
    /// hold two instruments, so instruments can't use SideBySide.)
    DuplicateTrack,
    Default,
    Skip,
}

impl Mode {
    pub fn long(self) -> &'static str {
        match self {
            Mode::Vst2State => "Keep settings (VST3 accepts the VST2 data)",
            Mode::Raw => "Keep settings: pass data as-is (try it)",
            Mode::SideBySide => "Default settings + keep old plug-in (off) to copy settings",
            Mode::DuplicateTrack => "Duplicate the track: VST3 on the copy, old instrument kept on the original (muted)",
            Mode::Default => "VST3 with default settings",
            Mode::Skip => "Don't convert",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Mode::Vst2State => "Settings kept (VST2-compatible VST3)",
            Mode::Raw => "Old data passed as-is — check in Live",
            Mode::SideBySide => "Defaults + old plug-in kept (off)",
            Mode::DuplicateTrack => "Track duplicated: VST3 copy + old kept (muted)",
            Mode::Default => "Default settings",
            Mode::Skip => "Not converted",
        }
    }
    pub fn from_name(s: &str) -> Option<Mode> {
        Some(match s {
            "vst2state" => Mode::Vst2State,
            "raw" => Mode::Raw,
            "sidebyside" => Mode::SideBySide,
            "duplicate" => Mode::DuplicateTrack,
            "default" => Mode::Default,
            "skip" => Mode::Skip,
            _ => return None,
        })
    }
}

const PRESET_COMMON: &[&str] = &[
    "OverwriteProtectionNumber", "MpeEnabled", "MpeSettings", "ParameterSettings", "IsOn",
    "PowerMacroControlIndex", "PowerMacroMappingRange", "IsFolded", "StoredAllParameters",
    "DeviceLomId", "DeviceViewLomId", "IsOnLomId", "ParametersListWrapperLomId",
];

// ----------------------------------------------------------------------------- installed VST3s

#[derive(Clone, Debug)]
pub struct Plugin {
    pub uid: [u8; 16],
    pub name: String,
    pub vendor: String,
    pub kind: String,
    pub dev_id: String,
}

impl Plugin {
    pub fn is_instrument(&self) -> bool {
        self.kind == "instr"
    }
}

pub fn live_plugin_dbs() -> Vec<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|b| PathBuf::from(b).join("Ableton").join("Live Database"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/Ableton/Live Database"))
    };
    let mut dbs: Vec<PathBuf> = base
        .and_then(|b| std::fs::read_dir(b).ok())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    n.starts_with("Live-plugins-") && n.ends_with(".db")
                })
                .collect()
        })
        .unwrap_or_default();
    dbs.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok()));
    dbs
}

fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    let bytes = hex_decode(&hex);
    bytes.try_into().ok()
}

/// Installed VST3 plug-ins, read from Live's own plug-in database (read-only).
pub fn load_catalog() -> Vec<Plugin> {
    let mut out = Vec::new();
    let Some(path) = live_plugin_dbs().into_iter().next() else { return out };
    let rows = crate::sqlite::Db::open(&path).map_err(|e| e.to_string()).and_then(|db| db.rows("plugins")).unwrap_or_default();
    let re = regex::Regex::new(r"^device:vst3:(\w+):([0-9a-fA-F-]{36})").unwrap();
    let mut seen = HashSet::new();
    let get = |r: &HashMap<String, crate::sqlite::Value>, k: &str| r.get(k).and_then(|v| v.text()).unwrap_or("").to_string();
    for r in rows {
        let (dev_id, name, vendor) = (get(&r, "dev_identifier"), get(&r, "name"), get(&r, "vendor"));
        let Some(c) = re.captures(&dev_id) else { continue };
        let Some(uid) = parse_uuid(&c[2]) else { continue };
        if seen.insert(uid) {
            out.push(Plugin { uid, name, vendor, kind: c[1].to_string(), dev_id });
        }
    }
    out
}

// ----------------------------------------------------------------------------- learned parameter IDs

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct LearnedEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub inputs: Option<String>,
    #[serde(default)]
    pub outputs: Option<String>,
    /// parameter key -> (VST3 parameter ID, ParameterIdFlankBool value)
    #[serde(default)]
    pub params: HashMap<String, (String, Option<String>)>,
}

fn learned_path() -> PathBuf {
    crate::config::app_dir().join("vst3_parameters.json")
}

fn learned() -> MutexGuard<'static, HashMap<String, LearnedEntry>> {
    static L: OnceLock<Mutex<HashMap<String, LearnedEntry>>> = OnceLock::new();
    L.get_or_init(|| {
        let m = std::fs::read_to_string(learned_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Mutex::new(m)
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

pub fn save_learned() {
    let s = serde_json::to_string(&*learned());
    if let Ok(s) = s {
        let tmp = learned_path().with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(tmp, learned_path());
        }
    }
}

/// Stable keys for a plug-in's parameters; repeated names get #2, #3…
fn param_keys(params: &[Node]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    params
        .iter()
        .map(|p| {
            let name = val(*p, "ParameterName").unwrap_or("").to_string();
            let n = seen.entry(name.clone()).or_default();
            *n += 1;
            if *n == 1 { name } else { format!("{name}#{n}") }
        })
        .collect()
}

pub fn uid_of(info: Node) -> Option<[u8; 16]> {
    let u = child(info, "Uid")?;
    let mut out = [0u8; 16];
    for (i, f) in elements(u).take(4).enumerate() {
        let v: i64 = f.attribute("Value")?.parse().ok()?;
        out[i * 4..i * 4 + 4].copy_from_slice(&((v as u32).to_be_bytes()));
    }
    Some(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Remember VST3 parameter IDs (and bus counts) from every VST3 in a set.
pub fn learn_from_doc(doc: &roxmltree::Document) -> usize {
    let mut db = learned();
    let mut added = 0;
    for pd in doc.descendants().filter(|n| n.is_element() && tag(*n) == "PluginDevice") {
        let Some(info) = find(pd, "PluginDesc/Vst3PluginInfo") else { continue };
        let Some(uid) = uid_of(info) else { continue };
        let e = db.entry(hex(&uid)).or_insert_with(|| LearnedEntry { name: val(info, "Name").unwrap_or("").into(), ..Default::default() });
        if let Some(v) = val(info, "NumAudioInputs") {
            e.inputs = Some(v.into());
        }
        if let Some(v) = val(info, "NumAudioOutputs") {
            e.outputs = Some(v.into());
        }
        let params: Vec<Node> = child(pd, "ParameterList").map(|l| elements(l).collect()).unwrap_or_default();
        for (p, k) in params.iter().zip(param_keys(&params)) {
            let Some(pid) = val(*p, "ParameterId") else { continue };
            if !e.params.contains_key(&k) {
                e.params.insert(k, (pid.to_string(), val(*p, "ParameterIdFlankBool").map(String::from)));
                added += 1;
            }
        }
    }
    added
}

fn walk_als(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_file() {
        if p.extension().map(|e| e.eq_ignore_ascii_case("als")).unwrap_or(false) {
            out.push(p.to_path_buf());
        }
        return;
    }
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() && path.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("backup")).unwrap_or(false) {
                continue;
            }
            walk_als(&path, out);
        }
    }
}

/// Scan sets/folders (skipping cloud-only files) and store what was learned.
pub fn learn_paths(paths: &[PathBuf], mut progress: impl FnMut(usize, usize)) -> usize {
    let mut files = Vec::new();
    for p in paths {
        walk_als(p, &mut files);
    }
    let mut total = 0;
    for (i, f) in files.iter().enumerate() {
        progress(i + 1, files.len());
        if als::is_cloud_only(f) {
            continue;
        }
        if let Ok(xml) = als::read_xml(f) {
            if let Ok(doc) = als::parse(&xml) {
                total += learn_from_doc(&doc);
            }
        }
    }
    save_learned();
    total
}

// ----------------------------------------------------------------------------- sources

#[derive(Clone, Debug)]
pub struct Source {
    pub fmt: String,
    pub name: String,
    /// VST2 unique ID (AU: subtype, which JUCE sets to the same code).
    pub uid4: Option<u32>,
    pub is_instrument: bool,
    pub chunk: Option<Vec<u8>>,
    /// "bank", "juce", "au-data", "params" or ""
    pub chunk_kind: String,
    pub version: i32,
    pub programs: i32,
    pub program: i32,
    pub shell: bool,
}

pub fn hex_decode(s: &str) -> Vec<u8> {
    let h: Vec<u8> = s.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    h.chunks_exact(2)
        .map(|c| {
            let d = |x: u8| (x as char).to_digit(16).unwrap() as u8;
            d(c[0]) << 4 | d(c[1])
        })
        .collect()
}

/// Integer or four-char code (e.g. "Ruum") -> u32.
fn int4(v: Option<&str>) -> Option<u32> {
    let v = v?.trim();
    if let Ok(i) = v.parse::<i64>() {
        return Some(i as u32);
    }
    let b = v.as_bytes();
    (b.len() == 4).then(|| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn vst2_source(info: Node) -> Source {
    let vp = find(info, "Preset/VstPreset");
    let ptype = vp.and_then(|p| int4(val(p, "Type")));
    let chunk = vp.and_then(|p| child(p, "Buffer")).map(|b| hex_decode(b.text().unwrap_or(""))).unwrap_or_default();
    let mut kind = match ptype {
        Some(0x4642_4368) | Some(0x4650_4368) => "bank", // 'FBCh' / 'FPCh'
        _ => "params",
    };
    if kind == "bank" && chunk.starts_with(b"VC2!") {
        kind = "juce";
    }
    let num = |n: Option<Node>, p: &str, d: i32| n.and_then(|x| val(x, p)).and_then(|v| v.parse::<i64>().ok()).map(|v| v as i32).unwrap_or(d);
    let name = val(info, "PlugName").filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| als::stem_of(val(info, "Path").unwrap_or("")));
    Source {
        fmt: "VST2".into(),
        name,
        uid4: int4(val(info, "UniqueId")),
        is_instrument: val(info, "Category") == Some("2"),
        chunk_kind: if chunk.is_empty() { String::new() } else { kind.into() },
        chunk: (!chunk.is_empty()).then_some(chunk),
        version: num(vp, "PluginVersion", num(Some(info), "Version", 0)),
        programs: num(vp, "ProgramCount", 1),
        program: num(vp, "ProgramNumber", 0),
        shell: val(info, "IsShellClient") == Some("true"),
    }
}

/// Audio Units: the state is a property list; JUCE puts its state under 'jucePluginState'.
fn au_source(info: Node) -> Source {
    let find_val = |names: &[&str]| {
        info.descendants()
            .find(|e| e.is_element() && names.contains(&tag(*e)) && e.attribute("Value").is_some())
            .and_then(|e| e.attribute("Value"))
    };
    let subtype = int4(find_val(&["ComponentSubType", "SubType"]));
    let ctype = int4(find_val(&["ComponentType", "Type"]));
    let raw = info
        .descendants()
        .filter(|e| e.is_element() && tag(*e) == "Buffer")
        .map(|e| hex_decode(e.text().unwrap_or("")))
        .find(|b| !b.is_empty())
        .unwrap_or_default();
    let (mut chunk, mut kind) = (None, "");
    if !raw.is_empty() {
        match plist::Value::from_reader(std::io::Cursor::new(&raw)) {
            Ok(v) => {
                if let Some(d) = v.as_dictionary() {
                    if let Some(b) = d.get("jucePluginState").and_then(|x| x.as_data()) {
                        chunk = Some(b.to_vec());
                        kind = "juce";
                    } else if let Some(b) = d.get("data").and_then(|x| x.as_data()) {
                        chunk = Some(b.to_vec());
                        kind = "au-data";
                    }
                }
            }
            Err(_) => {
                chunk = Some(raw);
                kind = "au-data";
            }
        }
    }
    Source {
        fmt: "AU".into(),
        name: find_val(&["Name", "PlugName"]).unwrap_or("Audio Unit").into(),
        uid4: subtype,
        is_instrument: ctype == Some(0x6175_6D75), // 'aumu'
        chunk,
        chunk_kind: kind.into(),
        version: 0,
        programs: 1,
        program: 0,
        shell: false,
    }
}

// ----------------------------------------------------------------------------- matching

pub fn normalize(name: &str) -> String {
    static RES: OnceLock<[regex::Regex; 4]> = OnceLock::new();
    let r = RES.get_or_init(|| {
        [
            regex::Regex::new(r"\((x64|x86|64[- ]?bit|stereo|mono|vst2?)\)").unwrap(),
            regex::Regex::new(r"[\s_-]*(x64|x86_64|64[- ]?bit|vst2?|64)\s*$").unwrap(),
            regex::Regex::new(r"[\s_-]*v?\d+(\.\d+)+\s*$").unwrap(),
            regex::Regex::new(r"[^a-z0-9]").unwrap(),
        ]
    });
    let n = name.to_lowercase();
    let n = r[0].replace_all(&n, " ");
    let n = r[1].replace(&n, "");
    let n = r[2].replace(&n, "");
    r[3].replace_all(&n, "").into_owned()
}

fn find_target(src: &Source, catalog: &[Plugin]) -> (Option<Plugin>, Option<&'static str>) {
    if let Some(id) = src.uid4 {
        let mut pre = b"VST".to_vec();
        pre.extend_from_slice(&id.to_be_bytes());
        let mut hits: Vec<&Plugin> = catalog.iter().filter(|p| p.uid[..7] == pre[..]).collect();
        if !hits.is_empty() {
            let tail: Vec<u8> = src.name.to_lowercase().bytes().filter(|b| b.is_ascii_alphanumeric() || *b == b' ').take(9).collect();
            hits.sort_by_key(|p| {
                let t: Vec<u8> = p.uid[7..].iter().copied().take_while(|b| *b != 0).collect();
                (t != tail, p.name.len())
            });
            return (Some(hits[0].clone()), Some("compat"));
        }
    }
    let n = normalize(&src.name);
    if !n.is_empty() {
        let mut hits: Vec<&Plugin> = catalog.iter().filter(|p| normalize(&p.name) == n).collect();
        hits.sort_by_key(|p| p.is_instrument() != src.is_instrument);
        if let Some(h) = hits.first() {
            return (Some((*h).clone()), Some("name"));
        }
    }
    (None, None)
}

fn default_mode(src: &Source, m: Option<&str>, instrument: bool, can_duplicate: bool) -> (Mode, &'static str) {
    match m {
        None => (Mode::Skip, "No VST3 with this ID or name is installed."),
        Some(_) if src.shell => (Mode::Skip, "Shell plug-in (e.g. Waves): not supported."),
        Some("compat") => {
            if matches!(src.chunk_kind.as_str(), "bank" | "juce") {
                (Mode::Vst2State, "The VST3 declares it replaces this VST2 — settings carry over.")
            } else if src.fmt == "AU" && src.chunk.is_some() {
                (Mode::Raw, "The VST3 replaces the VST2 with this ID; passing the AU state as-is.")
            } else {
                (Mode::Default, "The old plug-in stored no state; the VST3 starts from defaults.")
            }
        }
        _ if src.chunk_kind == "juce" => (Mode::Raw, "JUCE plug-in: its saved state is usually the same in every format."),
        // An instrument can't get a side-by-side copy: a chain holds only one instrument
        // (Live crashes opening a set with two in a chain). Duplicate its track instead.
        _ if instrument && can_duplicate => (
            Mode::DuplicateTrack,
            "Instrument with different plug-in IDs: its settings can't be converted. The track is duplicated: the copy \
             gets the VST3 (default settings), the original keeps the old instrument, muted, to copy settings from.",
        ),
        _ if instrument => (
            Mode::Skip,
            "Instrument with different plug-in IDs: its sound can't be converted, so it stays VST2. \
             Pick \"VST3 with default settings\" to replace it anyway.",
        ),
        _ => (Mode::SideBySide, "Different plug-in IDs: settings can't be converted automatically."),
    }
}

// ----------------------------------------------------------------------------- the plan

#[derive(Clone, Debug)]
struct ParamRec {
    key: String,
    remove: Range<usize>,
    id_value: Option<Range<usize>>,
    id_elem_end: usize,
    id_indent: String,
    has_flank: bool,
    target_ids: Vec<String>,
    mapped: bool,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub track: String,
    pub src: Source,
    pub target: Option<Plugin>,
    /// "compat" (VST3 declares it replaces the VST2) or "name".
    #[allow(dead_code)]
    pub match_kind: Option<&'static str>,
    pub mode: Mode,
    pub note: String,
    /// Keys of parameters with automation, modulation or macro/MIDI mappings.
    pub automated: Vec<String>,
    /// Those whose VST3 parameter ID is known.
    pub mapped: Vec<String>,
    device: Range<usize>,
    device_tag: String,
    info: Range<usize>,
    info_indent: String,
    info_id: String,
    win_x: String,
    win_y: String,
    preset_id: String,
    preset_common: Vec<String>,
    params: Vec<ParamRec>,
    bsc_device_ids: Vec<Range<usize>>,
    bsc_paths: Vec<Range<usize>>,
    clone_ids: Vec<Range<usize>>,
    on_value: Option<Range<usize>>,
    user_name_value: Option<Range<usize>>,
    device_id_value: Option<Range<usize>>,
    sibling_max_id: i64,
    /// The MIDI/audio track the plug-in is on (None on return/main tracks).
    track_info: Option<TrackInfo>,
}

/// Where a track is in the file, for duplicating it.
#[derive(Clone, Debug)]
struct TrackInfo {
    range: Range<usize>,
    id: String,
    /// Name to build the new names from (UserName, else EffectiveName).
    name: String,
    effective: String,
    effective_name_value: Option<Range<usize>>,
    user_name_value: Option<Range<usize>>,
    /// The track activator (DeviceChain/Mixer/Speaker/Manual).
    speaker_value: Option<Range<usize>>,
}

impl Item {
    pub fn old_label(&self) -> String {
        format!("{} [{}]", self.src.name, self.src.fmt)
    }
    pub fn new_label(&self) -> String {
        self.target.as_ref().map(|t| format!("{} [VST3]", t.name)).unwrap_or_else(|| "— no VST3 found —".into())
    }
    /// An instrument (on either side): it can't share a chain with another instrument.
    pub fn is_instrument(&self) -> bool {
        self.src.is_instrument || self.target.as_ref().map(|t| t.is_instrument()).unwrap_or(false)
    }

    pub fn modes(&self) -> Vec<Mode> {
        if self.target.is_none() {
            return vec![Mode::Skip];
        }
        let mut m = Vec::new();
        if self.src.chunk.is_some() && matches!(self.src.chunk_kind.as_str(), "bank" | "juce") {
            m.push(Mode::Vst2State);
        }
        if self.src.chunk.is_some() {
            m.push(Mode::Raw);
        }
        if !self.is_instrument() {
            m.push(Mode::SideBySide);
        } else if self.track_info.is_some() {
            m.push(Mode::DuplicateTrack);
        }
        m.extend([Mode::Default, Mode::Skip]);
        m
    }
}

pub struct Plan {
    pub path: PathBuf,
    xml: String,
    pub items: Vec<Item>,
    pub catalog_size: usize,
    envelopes: Vec<(String, Range<usize>)>,
    next_pointee: Option<(Range<usize>, i64)>,
    max_target_id: i64,
    /// Highest track Id, for new tracks.
    max_track_id: i64,
    /// Why this set can't be converted as it is (saved by a Live without VST3).
    pub blocked: Option<String>,
    /// Write VST3 entries the way the set's own Live version does.
    layout: Vst3Layout,
}

/// The optional fields a Live version writes in VST3 entries. Live added them
/// over time, and Live 12 can crash opening an older-format set that has a
/// field its version never wrote. Checked against ~700 real sets:
///
/// | Live (MinorVersion)           | bus counts | IsPlaceholderDevice | ParameterIdFlankBool |
/// |-------------------------------|------------|---------------------|----------------------|
/// | 10.1–11.1 (10.0_376–11.0_436) | no         | no                  | no                   |
/// | 11.2 (11.0_11202)             | no         | no                  | yes                  |
/// | 11.3 (11.0_11300)             | yes        | no                  | yes                  |
/// | 12 (12.0_…)                   | yes        | yes                 | yes                  |
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Vst3Layout {
    /// NumAudioInputs / NumAudioOutputs in Vst3PluginInfo
    pub bus_counts: bool,
    /// IsPlaceholderDevice in Vst3PluginInfo
    pub placeholder: bool,
    /// ParameterIdFlankBool after each ParameterId
    pub flank: bool,
}

/// First Live 10 file format that can hold VST3 plug-ins. VST3 arrived in Live 10.1:
/// all Live 10.0.x sets are "10.0_370"; the 10.1 betas wrote "10.0_376" and
/// Live 10.1 releases "10.0_377" (checked against real sets).
const FIRST_VST3_LIVE10_BUILD: u32 = 376;
/// Live 11.2 and 11.3 file formats ("11.0_11202", "11.0_11300").
const LIVE_11_2_BUILD: u32 = 11202;
const LIVE_11_3_BUILD: u32 = 11300;

/// The VST3 layout for a file format, from the root element: MajorVersion="4"
/// MinorVersion="9.5_327" is Live 9, "10.0_370" Live 10.0, "10.0_377" Live 10.1,
/// "11.0_436" Live 11.1, "11.0_11202" Live 11.2… None: the format can't hold VST3.
pub fn vst3_layout(major: Option<&str>, minor: Option<&str>) -> Option<Vst3Layout> {
    let major: u32 = major.and_then(|m| m.trim().parse().ok()).unwrap_or(0);
    let (version, build) = match minor {
        Some(m) => {
            let version: u32 = m.split(['.', '_']).next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            let build: Option<u32> = m.split('_').nth(1).and_then(|b| b.trim().parse().ok());
            (version, build)
        }
        None => (0, None),
    };
    if major < 5 || version < 10 {
        return None;
    }
    let build = build.unwrap_or(0);
    Some(match version {
        10 if build < FIRST_VST3_LIVE10_BUILD => return None, // Live 10.0
        10 => Vst3Layout::default(),
        11 if build < LIVE_11_2_BUILD => Vst3Layout::default(),
        11 if build < LIVE_11_3_BUILD => Vst3Layout { flank: true, ..Default::default() },
        11 => Vst3Layout { bus_counts: true, flank: true, ..Default::default() },
        _ => Vst3Layout { bus_counts: true, placeholder: true, flank: true },
    })
}

/// Correct the layout from VST3 entries Live itself already wrote in this set,
/// so the converted ones match them exactly (whatever the Live version).
fn mirror_layout(doc: &roxmltree::Document, mut layout: Vst3Layout) -> Vst3Layout {
    if let Some(info) = doc.descendants().find(|n| n.is_element() && tag(*n) == "Vst3PluginInfo") {
        layout.bus_counts = child(info, "NumAudioInputs").is_some();
        layout.placeholder = child(info, "IsPlaceholderDevice").is_some();
    }
    if doc.descendants().any(|n| n.is_element() && tag(n) == "ParameterIdFlankBool") {
        layout.flank = true;
    } else if doc.descendants().any(|pd| {
        pd.is_element()
            && tag(pd) == "PluginDevice"
            && find(pd, "PluginDesc/Vst3PluginInfo").is_some()
            && child(pd, "ParameterList").map(|l| elements(l).next().is_some()).unwrap_or(false)
    }) {
        layout.flank = false; // VST3 parameters written without it
    }
    layout
}

fn ws_start(xml: &str, mut i: usize) -> usize {
    let b = xml.as_bytes();
    while i > 0 && matches!(b[i - 1], b' ' | b'\t' | b'\r' | b'\n') {
        i -= 1;
    }
    i
}

fn line_indent(xml: &str, i: usize) -> String {
    let start = xml[..i].rfind('\n').map(|p| p + 1).unwrap_or(0);
    xml[start..i].chars().take_while(|c| *c == '\t' || *c == ' ').collect()
}

fn attr_range(n: Node, name: &str) -> Option<Range<usize>> {
    n.attributes().find(|a| a.name() == name).map(|a| a.range_value())
}

fn value_range(n: Node, path: &str) -> Option<Range<usize>> {
    attr_range(find(n, path)?, "Value")
}

fn param_target_ids(p: Node) -> Vec<String> {
    child(p, "ParameterValue")
        .map(|pv| {
            elements(pv)
                .filter(|e| matches!(tag(*e), "AutomationTarget" | "ModulationTarget"))
                .filter_map(|e| e.attribute("Id").map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn is_pointee_tag(t: &str) -> bool {
    matches!(t, "AutomationTarget" | "ModulationTarget" | "Pointee") || t.ends_with("ModulationTarget")
}

pub fn analyze(path: &Path, catalog: Option<Vec<Plugin>>) -> Result<Plan, String> {
    // Learn from the set and its siblings first: they may already use the VST3s.
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut sibs: Vec<PathBuf> = std::fs::read_dir(folder)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("als")).unwrap_or(false)).collect())
        .unwrap_or_default();
    sibs.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok()));
    for s in sibs.iter().take(6) {
        if s != path && !als::is_cloud_only(s) {
            if let Ok(x) = als::read_xml(s) {
                if let Ok(d) = als::parse(&x) {
                    learn_from_doc(&d);
                }
            }
        }
    }
    let xml = als::read_xml(path).map_err(|e| format!("Can't read the set: {e}"))?;
    let catalog = catalog.unwrap_or_else(load_catalog);
    let layout;
    let mut blocked = None;
    let (items, envelopes, next_pointee, max_target_id, max_track_id) = {
        let doc = als::parse(&xml)?;
        learn_from_doc(&doc);
        save_learned();
        let env_targets: HashSet<String> = doc
            .descendants()
            .filter(|n| n.is_element() && tag(*n) == "EnvelopeTarget")
            .filter_map(|n| val(n, "PointeeId").map(String::from))
            .collect();
        let mut envelopes = Vec::new();
        for et in doc.descendants().filter(|n| n.is_element() && tag(*n) == "EnvelopeTarget") {
            if let (Some(pid), Some(env)) = (val(et, "PointeeId"), et.parent_element()) {
                let r = env.range();
                envelopes.push((pid.to_string(), ws_start(&xml, r.start)..r.end));
            }
        }
        let root = doc.root_element();
        let by_version = vst3_layout(root.attribute("MajorVersion"), root.attribute("MinorVersion"));
        layout = mirror_layout(&doc, by_version.unwrap_or_default());
        if by_version.is_none() {
            let creator = root.attribute("Creator").unwrap_or("an old version of Ableton Live");
            blocked = Some(format!(
                "This set was last saved with {creator}, which had no VST3 support (VST3 arrived in Live 10.1), so its \
                 file can't hold VST3 plug-ins (Live would report the converted set as corrupt). Open it in your current \
                 Live and save it (the version shortcut saves it as the next version), then convert that new version."
            ));
        }
        let ls = child(root, "LiveSet").ok_or("Not a Live set (no LiveSet element)")?;
        let next_pointee = child(ls, "NextPointeeId").and_then(|n| {
            let r = attr_range(n, "Value")?;
            Some((r, n.attribute("Value")?.parse().ok()?))
        });
        let max_target_id = doc
            .descendants()
            .filter(|n| n.is_element() && tag(*n) == "AutomationTarget")
            .filter_map(|n| n.attribute("Id")?.parse::<i64>().ok())
            .max()
            .unwrap_or(0);
        let db = learned();
        let mut items = Vec::new();
        for dev in ls.descendants().filter(|n| n.is_element() && matches!(tag(*n), "PluginDevice" | "AuPluginDevice")) {
            let Some(desc) = child(dev, "PluginDesc") else { continue };
            let Some(info) = elements(desc).next() else { continue };
            let src = match tag(info) {
                "VstPluginInfo" => vst2_source(info),
                "AuPluginInfo" => au_source(info),
                _ => continue,
            };
            let track_node = dev.ancestors().find(|a| a.is_element() && tag(*a).ends_with("Track"));
            let track = track_node
                .map(|t| val(t, "Name/EffectiveName").map(String::from).unwrap_or_else(|| tag(t).replace("Track", "")))
                .unwrap_or_default();
            let track_info = track_node.filter(|t| matches!(tag(*t), "MidiTrack" | "AudioTrack")).and_then(|t| {
                let user = val(t, "Name/UserName").unwrap_or("");
                Some(TrackInfo {
                    range: t.range(),
                    id: t.attribute("Id")?.to_string(),
                    name: if user.trim().is_empty() { val(t, "Name/EffectiveName").unwrap_or("Track").to_string() } else { user.to_string() },
                    effective: val(t, "Name/EffectiveName").unwrap_or("Track").to_string(),
                    effective_name_value: value_range(t, "Name/EffectiveName"),
                    user_name_value: value_range(t, "Name/UserName"),
                    speaker_value: value_range(t, "DeviceChain/Mixer/Speaker/Manual"),
                })
            });
            let (target, match_kind) = find_target(&src, &catalog);
            let instrument = src.is_instrument || target.as_ref().map(|t| t.is_instrument()).unwrap_or(false);
            let (mode, note) = default_mode(&src, match_kind, instrument, track_info.is_some());
            let lmap = target.as_ref().and_then(|t| db.get(&hex(&t.uid)));
            let params: Vec<Node> = child(dev, "ParameterList").map(|l| elements(l).collect()).unwrap_or_default();
            let mut recs = Vec::new();
            let (mut automated, mut mapped) = (Vec::new(), Vec::new());
            for (p, key) in params.iter().zip(param_keys(&params)) {
                let target_ids = param_target_ids(*p);
                let has_midi = find(*p, "ParameterValue/KeyMidi").is_some();
                let is_mapped = has_midi || target_ids.iter().any(|t| env_targets.contains(t));
                if is_mapped {
                    automated.push(key.clone());
                    if lmap.map(|m| m.params.contains_key(&key)).unwrap_or(false) {
                        mapped.push(key.clone());
                    }
                }
                let pid = child(*p, "ParameterId");
                let r = p.range();
                recs.push(ParamRec {
                    key,
                    remove: ws_start(&xml, r.start)..r.end,
                    id_value: pid.and_then(|n| attr_range(n, "Value")),
                    id_elem_end: pid.map(|n| n.range().end).unwrap_or(r.end),
                    id_indent: pid.map(|n| line_indent(&xml, n.range().start)).unwrap_or_default(),
                    has_flank: child(*p, "ParameterIdFlankBool").is_some(),
                    target_ids,
                    mapped: is_mapped,
                });
            }
            let preset = info.descendants().find(|e| e.is_element() && matches!(tag(*e), "VstPreset" | "AuPreset"));
            let preset_common = preset
                .map(|p| PRESET_COMMON.iter().filter_map(|t| child(p, t)).map(|c| xml[c.range()].to_string()).collect())
                .unwrap_or_default();
            let mut bsc_device_ids = Vec::new();
            let mut bsc_paths = Vec::new();
            for b in dev.descendants().filter(|n| n.is_element() && tag(*n) == "BranchSourceContext") {
                bsc_device_ids.extend(value_range(b, "BranchDeviceId"));
                bsc_paths.extend(value_range(b, "BrowserContentPath"));
            }
            let clone_ids = dev
                .descendants()
                .filter(|n| n.is_element() && is_pointee_tag(tag(*n)))
                .filter_map(|n| attr_range(n, "Id"))
                .collect();
            let sibling_max_id = dev
                .parent_element()
                .map(|p| elements(p).filter_map(|c| c.attribute("Id")?.parse::<i64>().ok()).max().unwrap_or(0))
                .unwrap_or(0);
            items.push(Item {
                track,
                src,
                target,
                match_kind,
                mode,
                note: note.into(),
                automated,
                mapped,
                device: dev.range(),
                device_tag: tag(dev).into(),
                info: info.range(),
                info_indent: line_indent(&xml, info.range().start),
                info_id: info.attribute("Id").unwrap_or("0").into(),
                win_x: val(info, "WinPosX").unwrap_or("-1").into(),
                win_y: val(info, "WinPosY").unwrap_or("-1").into(),
                preset_id: preset.and_then(|p| p.attribute("Id")).unwrap_or("0").into(),
                preset_common,
                params: recs,
                bsc_device_ids,
                bsc_paths,
                clone_ids,
                on_value: value_range(dev, "On/Manual"),
                user_name_value: value_range(dev, "UserName"),
                device_id_value: attr_range(dev, "Id"),
                sibling_max_id,
                track_info,
            });
        }
        let max_track_id = child(ls, "Tracks")
            .map(|ts| elements(ts).filter_map(|t| t.attribute("Id")?.parse::<i64>().ok()).max().unwrap_or(0))
            .unwrap_or(0);
        (items, envelopes, next_pointee, max_target_id, max_track_id)
    };
    Ok(Plan {
        path: path.to_path_buf(),
        xml,
        items,
        catalog_size: catalog.len(),
        envelopes,
        next_pointee,
        max_target_id,
        max_track_id,
        blocked,
        layout,
    })
}

// ----------------------------------------------------------------------------- building XML

/// VST2 chunk -> 'VstW' + 'CcnK'/'FBCh' bank (how Cubase and Live hand VST2 state to a VST3).
pub fn wrap_vst2_bank(chunk: &[u8], fx_id: u32, fx_version: i32, programs: i32, program: i32) -> Vec<u8> {
    let mut bank = Vec::with_capacity(160 + chunk.len());
    bank.extend_from_slice(b"CcnK");
    bank.extend_from_slice(&0u32.to_be_bytes()); // size, patched below
    bank.extend_from_slice(b"FBCh");
    bank.extend_from_slice(&2i32.to_be_bytes());
    bank.extend_from_slice(&fx_id.to_be_bytes());
    bank.extend_from_slice(&fx_version.to_be_bytes());
    bank.extend_from_slice(&programs.to_be_bytes());
    bank.extend_from_slice(&program.to_be_bytes());
    bank.extend_from_slice(&[0u8; 124]);
    bank.extend_from_slice(&(chunk.len() as i32).to_be_bytes());
    bank.extend_from_slice(chunk);
    let size = (bank.len() - 8) as i32;
    bank[4..8].copy_from_slice(&size.to_be_bytes());
    let mut out = Vec::with_capacity(16 + bank.len());
    out.extend_from_slice(b"VstW");
    for v in [8i32, 1, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend(bank);
    out
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn uid_xml(uid: &[u8; 16], ind: &str, nl: &str) -> String {
    let mut s = format!("<Uid>{nl}");
    for i in 0..4 {
        let v = i32::from_be_bytes(uid[i * 4..i * 4 + 4].try_into().unwrap());
        s += &format!("{ind}\t<Fields.{i} Value=\"{v}\" />{nl}");
    }
    s + &format!("{ind}</Uid>")
}

fn build_vst3_info(item: &Item, target: &Plugin, state: &[u8], nl: &str, layout: Vst3Layout) -> String {
    let db = learned();
    let lent = db.get(&hex(&target.uid));
    let inst = target.is_instrument();
    let i0 = &item.info_indent;
    let (i1, i2, i3, i4) = (format!("{i0}\t"), format!("{i0}\t\t"), format!("{i0}\t\t\t"), format!("{i0}\t\t\t\t"));
    let inputs = lent.and_then(|l| l.inputs.clone()).unwrap_or_else(|| if inst { "0".into() } else { "1".into() });
    let outputs = lent.and_then(|l| l.outputs.clone()).unwrap_or_else(|| "1".into());
    let dtype = if inst { "1" } else { "2" };
    let mut s = format!("<Vst3PluginInfo Id=\"{}\">{nl}", esc(&item.info_id));
    s += &format!("{i1}<WinPosX Value=\"{}\" />{nl}", esc(&item.win_x));
    s += &format!("{i1}<WinPosY Value=\"{}\" />{nl}", esc(&item.win_y));
    // Only the fields this set's Live version writes (see Vst3Layout).
    if layout.bus_counts {
        s += &format!("{i1}<NumAudioInputs Value=\"{}\" />{nl}", esc(&inputs));
        s += &format!("{i1}<NumAudioOutputs Value=\"{}\" />{nl}", esc(&outputs));
    }
    if layout.placeholder {
        s += &format!("{i1}<IsPlaceholderDevice Value=\"false\" />{nl}");
    }
    s += &format!("{i1}<Preset>{nl}{i2}<Vst3Preset Id=\"{}\">{nl}", esc(&item.preset_id));
    for c in &item.preset_common {
        s += &format!("{i3}{c}{nl}");
    }
    s += &format!("{i3}{}{nl}", uid_xml(&target.uid, &i3, nl));
    s += &format!("{i3}<DeviceType Value=\"{dtype}\" />{nl}");
    if state.is_empty() {
        s += &format!("{i3}<ProcessorState />{nl}");
    } else {
        let h: String = state.iter().map(|b| format!("{b:02X}")).collect();
        s += &format!("{i3}<ProcessorState>{nl}");
        for chunk in h.as_bytes().chunks(80) {
            s += &format!("{i4}{}{nl}", std::str::from_utf8(chunk).unwrap());
        }
        s += &format!("{i3}</ProcessorState>{nl}");
    }
    s += &format!("{i3}<ControllerState />{nl}{i3}<Name Value=\"\" />{nl}{i3}<PresetRef />{nl}");
    s += &format!("{i2}</Vst3Preset>{nl}{i1}</Preset>{nl}");
    s += &format!("{i1}<Name Value=\"{}\" />{nl}", esc(&target.name));
    s += &format!("{i1}{}{nl}", uid_xml(&target.uid, &i1, nl));
    s += &format!("{i1}<DeviceType Value=\"{dtype}\" />{nl}");
    s + &format!("{i0}</Vst3PluginInfo>")
}

fn state_for(item: &Item) -> Vec<u8> {
    let s = &item.src;
    match (item.mode, &s.chunk) {
        (Mode::Vst2State, Some(c)) => wrap_vst2_bank(c, s.uid4.unwrap_or(0), s.version, s.programs, s.program),
        (Mode::Raw, Some(c)) => c.clone(),
        _ => Vec::new(),
    }
}

/// Give every automation/modulation target in a copied track a fresh Id, and point
/// the copy's own automation envelopes at the new Ids (two-pass, so envelopes that
/// come before their targets in the text are mapped too).
fn renumber_pointees(text: &str, next_id: &mut i64) -> String {
    static DEF: OnceLock<regex::Regex> = OnceLock::new();
    static REF: OnceLock<regex::Regex> = OnceLock::new();
    let def = DEF.get_or_init(|| regex::Regex::new(r#"<(AutomationTarget|ModulationTarget|Pointee|[A-Za-z]+ModulationTarget) Id="(\d+)""#).unwrap());
    let refr = REF.get_or_init(|| regex::Regex::new(r#"<PointeeId Value="(\d+)""#).unwrap());
    let mut map: HashMap<String, i64> = HashMap::new();
    for c in def.captures_iter(text) {
        map.entry(c[2].to_string()).or_insert_with(|| {
            let v = *next_id;
            *next_id += 1;
            v
        });
    }
    let renamed = def.replace_all(text, |c: &regex::Captures| format!("<{} Id=\"{}\"", &c[1], map[&c[2]]));
    refr.replace_all(&renamed, |c: &regex::Captures| match map.get(&c[1]) {
        Some(v) => format!("<PointeeId Value=\"{v}\""),
        None => c[0].to_string(),
    })
    .into_owned()
}

/// New track Id and name for a copied track (its start tag and its Name block,
/// which comes before any devices).
fn set_track_identity(text: &str, id: i64, user_name: &str, effective_name: &str) -> String {
    let id_re = regex::Regex::new(r#"^(<\w+ Id=")\d+(")"#).unwrap();
    let text = id_re.replace(text, |c: &regex::Captures| format!("{}{id}{}", &c[1], &c[2]));
    let eff = regex::Regex::new(r#"<EffectiveName Value="[^"]*""#).unwrap();
    let text = eff.replace(&text, |_: &regex::Captures| format!("<EffectiveName Value=\"{}\"", esc(effective_name)));
    let user = regex::Regex::new(r#"<UserName Value="[^"]*""#).unwrap();
    user.replace(&text, |_: &regex::Captures| format!("<UserName Value=\"{}\"", esc(user_name))).into_owned()
}

/// Apply non-overlapping (range, replacement) edits to a text.
fn apply_edits(text: &str, mut edits: Vec<(Range<usize>, String)>) -> Result<String, String> {
    edits.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out = String::with_capacity(text.len() + 4096);
    let mut pos = 0;
    for (r, rep) in edits {
        if r.start < pos {
            return Err(format!("internal error: overlapping edits at byte {}", r.start));
        }
        out.push_str(&text[pos..r.start]);
        out.push_str(&rep);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    Ok(out)
}

/// The ParameterIdFlankBool value to add after a remapped ParameterId, if any.
/// Only Live 11.2+ files have this field (the learned value comes from such
/// sets); Live 12 crashes right after opening an older-format set that has it.
fn flank_to_add(layout: Vst3Layout, learned: Option<&str>, already_there: bool) -> Option<&str> {
    match (layout.flank, learned, already_there) {
        (true, Some(f), false) => Some(f),
        _ => None,
    }
}

/// Automated or mapped parameters whose VST3 ID isn't known: converting drops
/// their automation (rather than letting it drive the wrong parameter).
fn lost_params(item: &Item, db: &HashMap<String, LearnedEntry>) -> Vec<String> {
    let Some(target) = item.target.as_ref().filter(|_| item.mode != Mode::Skip) else { return vec![] };
    let lmap = db.get(&hex(&target.uid)).map(|e| &e.params);
    item.params
        .iter()
        .filter(|p| p.mapped && !lmap.map(|m| m.contains_key(&p.key)).unwrap_or(false))
        .map(|p| p.key.clone())
        .collect()
}

/// Report lines for one plug-in. Used both by the preview and after converting,
/// so the preview says exactly what converting will do.
fn item_report(item: &Item, lost: &[String]) -> Vec<String> {
    if item.target.is_none() || item.mode == Mode::Skip {
        return vec![format!("  · {}: {} — kept ({})", item.track, item.old_label(), item.note)];
    }
    let dup;
    let what = match item.mode {
        Mode::Vst2State => "settings kept",
        Mode::Raw => "settings passed as-is — check it",
        Mode::SideBySide => "default settings; old plug-in kept next to it (off)",
        Mode::DuplicateTrack => {
            dup = format!(
                "track duplicated: \"{} (VST3)\" gets the VST3 with default settings, the original keeps the old instrument (muted)",
                item.track
            );
            dup.as_str()
        }
        _ => "default settings",
    };
    let mut line = format!("  ✓ {}: {} → {} — {what}", item.track, item.old_label(), item.new_label());
    if !item.automated.is_empty() {
        line += &format!("; automation/mappings kept for {}/{} parameters", item.automated.len() - lost.len(), item.automated.len());
    }
    let mut out = vec![line];
    if !lost.is_empty() {
        let mut shown = lost.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        if lost.len() > 6 {
            shown += "…";
        }
        let where_ = if item.mode == Mode::DuplicateTrack { "; the original track keeps it" } else { "" };
        out.push(format!("      lost automation for: {shown} (VST3 parameter IDs not learned yet{where_})"));
    }
    out
}

/// What converting would do with the current choices, without changing anything:
/// a short summary followed by the same per-plug-in lines the conversion reports.
pub fn preview_report(plan: &Plan) -> Vec<String> {
    let db = learned().clone();
    let set = plan.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let n = plan.items.len();
    if n == 0 {
        return vec![format!("{set} has no VST2 or Audio Unit plug-ins. Nothing to convert.")];
    }
    let mut out = Vec::new();
    if let Some(why) = &plan.blocked {
        out.push(format!("Can't convert {set} yet."));
        out.push(format!("  {why}"));
        out.push("  What converting would do once it's saved in a current Live:".into());
        out.push(String::new());
    }
    let count = |m: Mode| plan.items.iter().filter(|i| i.target.is_some() && i.mode == m).count();
    let conv = n - plan.items.iter().filter(|i| i.target.is_none() || i.mode == Mode::Skip).count();
    out.push(format!("Plug-in report for {set}: {conv} of {n} VST2/AU plug-ins can be converted to VST3"));
    let parts: Vec<String> = [
        (Mode::Vst2State, "keeps its settings", "keep their settings"),
        (Mode::Raw, "passes its data as-is (check in Live)", "pass their data as-is (check in Live)"),
        (Mode::SideBySide, "starts from defaults with the old plug-in kept (off)", "start from defaults with the old plug-in kept (off)"),
        (Mode::DuplicateTrack, "gets a duplicated track (VST3 copy, old one muted)", "get duplicated tracks (VST3 copy, old one muted)"),
        (Mode::Default, "starts from defaults", "start from defaults"),
    ]
    .iter()
    .filter_map(|(m, one, many)| {
        let c = count(*m);
        (c > 0).then(|| format!("{c} {}", if c == 1 { one } else { many }))
    })
    .collect();
    if !parts.is_empty() {
        out.push(format!("  {}", parts.join(" · ")));
    }
    let (auto, lost): (usize, usize) = plan
        .items
        .iter()
        .filter(|i| i.target.is_some() && i.mode != Mode::Skip)
        .map(|i| (i.automated.len(), lost_params(i, &db).len()))
        .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    if auto > 0 {
        let noun = if auto == 1 { "parameter" } else { "parameters" };
        out.push(format!("  Automation/mappings: {} of {auto} {noun} would be kept", auto - lost));
    }
    out.push("  Nothing has been changed yet.".into());
    out.push(String::new());
    for item in &plan.items {
        out.extend(item_report(item, &lost_params(item, &db)));
    }
    out
}

/// Apply the plan to a copy of the set. Returns (output path, report lines).
pub fn convert(plan: &Plan, out_path: Option<PathBuf>) -> Result<(PathBuf, Vec<String>), String> {
    let xml = &plan.xml;
    let nl = if xml.contains("\r\n") { "\r\n" } else { "\n" };
    let mut next_id = plan.next_pointee.as_ref().map(|(_, v)| *v).unwrap_or(0).max(plan.max_target_id + 1);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    if let Some(why) = &plan.blocked {
        return Err(why.clone());
    }
    let mut dropped: HashSet<String> = HashSet::new();
    let mut report = Vec::new();
    let mut done = 0;
    let db = learned().clone();

    for item in &plan.items {
        report.extend(item_report(item, &lost_params(item, &db)));
        let Some(target) = item.target.as_ref().filter(|_| item.mode != Mode::Skip) else {
            continue;
        };
        let dev = item.device.clone();

        if item.mode == Mode::SideBySide && item.is_instrument() {
            // Never write two instruments into one chain: Live crashes opening such a set.
            return Err(format!("{}: an instrument can't be kept side by side with its VST3 (one instrument per chain).", item.old_label()));
        }
        if item.mode == Mode::SideBySide {
            // Copy of the untouched old device, switched off, with fresh automation IDs.
            let mut sub = Vec::new();
            for r in &item.clone_ids {
                sub.push((r.start - dev.start..r.end - dev.start, next_id.to_string()));
                next_id += 1;
            }
            if let Some(r) = &item.on_value {
                sub.push((r.start - dev.start..r.end - dev.start, "false".into()));
            }
            if let Some(r) = &item.user_name_value {
                sub.push((r.start - dev.start..r.end - dev.start, esc(&format!("{} (old {}, off)", item.src.name, item.src.fmt))));
            }
            if let Some(r) = &item.device_id_value {
                sub.push((r.start - dev.start..r.end - dev.start, (item.sibling_max_id + 1).to_string()));
            }
            let clone = apply_edits(&xml[dev.clone()], sub)?;
            let indent = line_indent(xml, dev.start);
            edits.push((dev.end..dev.end, format!("{nl}{indent}{clone}")));
        }

        edits.push((item.info.clone(), build_vst3_info(item, target, &state_for(item), nl, plan.layout)));
        if item.device_tag != "PluginDevice" {
            let t = &item.device_tag;
            edits.push((dev.start + 1..dev.start + 1 + t.len(), "PluginDevice".into()));
            if xml[dev.clone()].ends_with(&format!("</{t}>")) {
                edits.push((dev.end - t.len() - 1..dev.end - 1, "PluginDevice".into()));
            }
        }
        // Point Live's browser link at the VST3.
        for r in &item.bsc_device_ids {
            edits.push((r.clone(), esc(&target.dev_id)));
        }
        for r in &item.bsc_paths {
            edits.push((r.clone(), esc(&format!("query:Plugins#VST3:{}:{}", target.vendor, target.name))));
        }
        // Re-key parameters to VST3 IDs; drop the ones we can't map.
        let lmap = db.get(&hex(&target.uid)).map(|e| &e.params);
        for p in &item.params {
            match lmap.and_then(|m| m.get(&p.key)) {
                Some((pid, flank)) => {
                    if let Some(r) = &p.id_value {
                        edits.push((r.clone(), esc(pid)));
                    }
                    if let Some(f) = flank_to_add(plan.layout, flank.as_deref(), p.has_flank) {
                        edits.push((p.id_elem_end..p.id_elem_end, format!("{nl}{}<ParameterIdFlankBool Value=\"{}\" />", p.id_indent, esc(f))));
                    }
                }
                None => {
                    dropped.extend(p.target_ids.iter().cloned());
                    edits.push((p.remove.clone(), String::new()));
                }
            }
        }
        done += 1;
    }

    // Remove envelopes that pointed at parameters we dropped.
    for (pid, r) in &plan.envelopes {
        if dropped.contains(pid) {
            edits.push((r.clone(), String::new()));
        }
    }

    // Instruments in DuplicateTrack mode: every edit inside their track goes to a
    // copy of the track (which plays); the original keeps the old instrument and
    // everything else as it was, and is muted and renamed.
    let mut dups: Vec<(&TrackInfo, &str)> = Vec::new();
    for item in plan.items.iter().filter(|i| i.mode == Mode::DuplicateTrack && i.target.is_some()) {
        let t = item.track_info.as_ref().ok_or_else(|| format!("{}: no track to duplicate", item.old_label()))?;
        if !dups.iter().any(|(d, _)| d.range == t.range) {
            dups.push((t, item.src.fmt.as_str()));
        }
    }
    let mut next_track_id = plan.max_track_id;
    for (t, fmt) in dups {
        // Routings on other tracks (sidechains, resampling…) keep pointing at the original.
        let routed = xml.match_indices(&format!("Track.{}/", t.id)).any(|(i, _)| i < t.range.start || i > t.range.end);
        if routed {
            report.push(format!(
                "      note: other tracks route audio to or from \"{}\"; those routings still use the original (muted) track",
                t.effective
            ));
        }
        let (inside, rest): (Vec<_>, Vec<_>) =
            edits.into_iter().partition(|(r, _)| t.range.start <= r.start && r.end <= t.range.end);
        edits = rest;
        let shifted = inside.into_iter().map(|(r, s)| (r.start - t.range.start..r.end - t.range.start, s)).collect();
        let copy = apply_edits(&xml[t.range.clone()], shifted)?;
        let copy = renumber_pointees(&copy, &mut next_id);
        next_track_id += 1;
        let copy = set_track_identity(&copy, next_track_id, &format!("{} (VST3)", t.name), &format!("{} (VST3)", t.effective));
        let indent = line_indent(xml, t.range.start);
        edits.push((t.range.end..t.range.end, format!("{nl}{indent}{copy}")));
        let old = format!(" (old {fmt}, muted)");
        if let Some(r) = &t.user_name_value {
            edits.push((r.clone(), esc(&format!("{}{old}", t.name))));
        }
        if let Some(r) = &t.effective_name_value {
            edits.push((r.clone(), esc(&format!("{}{old}", t.effective))));
        }
        if let Some(r) = &t.speaker_value {
            edits.push((r.clone(), "false".into()));
        }
    }

    if let Some((r, v)) = &plan.next_pointee {
        edits.push((r.clone(), next_id.max(*v).to_string()));
    }
    let new_xml = apply_edits(xml, edits)?;
    // Never write something Live can't read.
    als::parse(&new_xml).map_err(|e| format!("internal error, the converted set didn't validate: {e}"))?;
    let out = out_path.unwrap_or_else(|| crate::versions::next_version_path(&plan.path));
    als::write_als(&out, &new_xml).map_err(|e| format!("Can't write {}: {e}", out.display()))?;
    let mut lines = vec![format!(
        "Converted plug-ins to VST3 ({done} of {}) from {}:",
        plan.items.len(),
        plan.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    )];
    lines.extend(report);
    Ok((out, lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same header Live wrote for ValhallaRoom's VST3 (taken from a real set).
    #[test]
    fn vst2_bank_matches_live() {
        let chunk = vec![0xAB; 704];
        let s = wrap_vst2_bank(&chunk, u32::from_be_bytes(*b"Ruum"), 67074, 0, 0);
        assert_eq!(&s[..16], &[0x56, 0x73, 0x74, 0x57, 0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(&s[16..20], b"CcnK");
        assert_eq!(i32::from_be_bytes(s[20..24].try_into().unwrap()), 856); // bank size Live wrote
        assert_eq!(&s[24..28], b"FBCh");
        assert_eq!(&s[32..36], b"Ruum");
        assert_eq!(i32::from_be_bytes(s[172..176].try_into().unwrap()), 704);
        assert_eq!(&s[176..], &chunk[..]);
        assert_eq!(s.len(), 880);
    }

    #[test]
    fn name_normalization() {
        assert_eq!(normalize("Spire-1.5"), normalize("Spire"));
        assert_eq!(normalize("Endless Smile 64"), normalize("Endless Smile"));
        assert_eq!(normalize("ValhallaRoom_x64"), normalize("ValhallaRoom"));
        assert_ne!(normalize("Kick 2 x64"), normalize("Kick 3"));
    }

    /// Live 12 crashed opening converted Live 10/11 sets that had ParameterIdFlankBool.
    #[test]
    fn flank_field_only_where_live_writes_it() {
        let with = Vst3Layout { flank: true, ..Default::default() };
        assert_eq!(flank_to_add(with, Some("false"), false), Some("false"));
        assert_eq!(flank_to_add(with, Some("false"), true), None, "already there");
        assert_eq!(flank_to_add(with, None, false), None);
        assert_eq!(flank_to_add(Vst3Layout::default(), Some("false"), false), None, "Live 10.1-11.1 never have it");
    }

    #[test]
    fn set_formats() {
        let l = |minor: &str| vst3_layout(Some("5"), Some(minor));
        let none = Vst3Layout::default();
        assert_eq!(vst3_layout(Some("4"), Some("9.5_327")), None); // Live 9.7
        assert_eq!(l("10.0_370"), None); // Live 10.0.x: no VST3 yet
        assert_eq!(l("10.0"), None); // unknown build: be safe
        assert_eq!(l("10.0_376"), Some(none)); // Live 10.1 beta
        assert_eq!(l("10.0_377"), Some(none)); // Live 10.1
        assert_eq!(l("11.0_433"), Some(none)); // Live 11.0 beta (the crashing set)
        assert_eq!(l("11.0_436"), Some(none)); // Live 11.1
        assert_eq!(l("11.0_11202"), Some(Vst3Layout { flank: true, ..none })); // Live 11.2
        assert_eq!(l("11.0_11300"), Some(Vst3Layout { bus_counts: true, flank: true, ..none })); // Live 11.3
        assert_eq!(l("12.0_12402"), Some(Vst3Layout { bus_counts: true, placeholder: true, flank: true }));
        assert_eq!(vst3_layout(None, None), None);
    }

    /// A minimal set with one ValhallaRoom VST2, saved as the given Live version.
    fn tiny_set(dir: &Path, name: &str, major: &str, minor: &str, creator: &str) -> PathBuf {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="{major}" MinorVersion="{minor}" Creator="{creator}">
	<LiveSet>
		<NextPointeeId Value="100" />
		<Tracks>
			<AudioTrack Id="1">
				<Name>
					<EffectiveName Value="Verb" />
				</Name>
				<DeviceChain>
					<DeviceChain>
						<Devices>
							<PluginDevice Id="0">
								<On>
									<Manual Value="true" />
									<AutomationTarget Id="10" />
								</On>
								<UserName Value="" />
								<PluginDesc>
									<VstPluginInfo Id="0">
										<WinPosX Value="1" />
										<WinPosY Value="2" />
										<PlugName Value="ValhallaRoom_x64" />
										<UniqueId Value="1383429485" />
										<Category Value="1" />
										<Preset>
											<VstPreset Id="5">
												<IsOn Value="true" />
												<Type Value="1178747752" />
												<ProgramCount Value="1" />
												<ProgramNumber Value="0" />
												<Buffer>56433221</Buffer>
												<PluginVersion Value="65545" />
											</VstPreset>
										</Preset>
									</VstPluginInfo>
								</PluginDesc>
								<ParameterList />
							</PluginDevice>
						</Devices>
					</DeviceChain>
				</DeviceChain>
			</AudioTrack>
		</Tracks>
	</LiveSet>
</Ableton>
"#
        );
        let p = dir.join(name);
        als::write_als(&p, &xml).unwrap();
        p
    }

    /// Live crashed opening sets where Spire (an instrument) had been kept side by
    /// side with its VST3: two instruments in one chain.
    #[test]
    fn instruments_are_never_copied_side_by_side() {
        let dir = std::env::temp_dir().join(format!("lv-inst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = tiny_set(&dir, "Synth.als", "5", "12.0_12402", "Ableton Live 12.4.16");
        // Turn the effect into a Spire-like instrument: VST2 category 2, non-JUCE state.
        let xml = als::read_xml(&p)
            .unwrap()
            .replace("ValhallaRoom_x64", "Spire-1.5")
            .replace("1383429485", "1399878194")
            .replace("<Category Value=\"1\" />", "<Category Value=\"2\" />")
            .replace("<Buffer>56433221</Buffer>", "<Buffer>00112233</Buffer>");
        als::write_als(&p, &xml).unwrap();
        let spire = vec![Plugin { uid: [0x2d; 16], name: "Spire".into(), vendor: "Reveal Sound".into(), kind: "instr".into(), dev_id: "device:vst3:instr:x".into() }];
        let mut plan = analyze(&p, Some(spire)).unwrap();
        let it = &plan.items[0];
        assert_eq!(it.match_kind, Some("name"));
        assert!(it.is_instrument());
        assert_eq!(it.mode, Mode::DuplicateTrack, "duplicates its track by default");
        assert!(!it.modes().contains(&Mode::SideBySide));
        assert!(it.modes().contains(&Mode::Default));
        assert!(it.modes().contains(&Mode::Skip));
        // Forcing a side-by-side copy anyway is refused, nothing written.
        let out = dir.join("Synth v_2.als");
        plan.items[0].mode = Mode::SideBySide;
        assert!(convert(&plan, Some(out.clone())).is_err());
        assert!(!out.exists());
        // Default settings: replaced, one instrument in the chain.
        plan.items[0].mode = Mode::Default;
        convert(&plan, Some(out.clone())).unwrap();
        let xml = als::read_xml(&out).unwrap();
        assert_eq!(xml.matches("<PluginDevice ").count(), 1);
        assert!(xml.contains("<Vst3PluginInfo"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duplicate-track mode: the original keeps the old instrument (muted, renamed,
    /// otherwise untouched); the copy gets the VST3, fresh IDs, and its own automation.
    #[test]
    fn instrument_track_is_duplicated() {
        let dir = std::env::temp_dir().join(format!("lv-dup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12402" Creator="Ableton Live 12.4.16">
	<LiveSet>
		<NextPointeeId Value="100" />
		<Tracks>
			<MidiTrack Id="5">
				<Name>
					<EffectiveName Value="Bass" />
					<UserName Value="Bass" />
				</Name>
				<AutomationEnvelopes>
					<Envelopes>
						<AutomationEnvelope Id="0">
							<EnvelopeTarget>
								<PointeeId Value="10" />
							</EnvelopeTarget>
						</AutomationEnvelope>
					</Envelopes>
				</AutomationEnvelopes>
				<DeviceChain>
					<Mixer>
						<Speaker>
							<Manual Value="true" />
							<AutomationTarget Id="20" />
						</Speaker>
					</Mixer>
					<DeviceChain>
						<Devices>
							<PluginDevice Id="0">
								<On>
									<Manual Value="true" />
									<AutomationTarget Id="10" />
								</On>
								<UserName Value="" />
								<PluginDesc>
									<VstPluginInfo Id="0">
										<PlugName Value="Spire-1.5" />
										<UniqueId Value="1399878194" />
										<Category Value="2" />
										<Preset>
											<VstPreset Id="5">
												<Type Value="1178747752" />
												<Buffer>00112233</Buffer>
											</VstPreset>
										</Preset>
									</VstPluginInfo>
								</PluginDesc>
								<ParameterList />
							</PluginDevice>
						</Devices>
					</DeviceChain>
				</DeviceChain>
			</MidiTrack>
			<AudioTrack Id="7">
				<Name>
					<EffectiveName Value="Other" />
				</Name>
			</AudioTrack>
		</Tracks>
	</LiveSet>
</Ableton>
"#;
        let p = dir.join("Song.als");
        als::write_als(&p, xml).unwrap();
        let spire = vec![Plugin { uid: [0x2d; 16], name: "Spire".into(), vendor: "Reveal Sound".into(), kind: "instr".into(), dev_id: "device:vst3:instr:x".into() }];
        let plan = analyze(&p, Some(spire)).unwrap();
        assert_eq!(plan.items[0].mode, Mode::DuplicateTrack);
        let out = dir.join("Song v_2.als");
        let (_, report) = convert(&plan, Some(out.clone())).unwrap();
        assert!(report.iter().any(|l| l.contains("track duplicated")));
        let text = als::read_xml(&out).unwrap();
        let doc = als::parse(&text).unwrap();
        let tracks: Vec<Node> = elements(find(doc.root_element(), "LiveSet/Tracks").unwrap()).collect();
        assert_eq!(tracks.iter().map(|t| (tag(*t), t.attribute("Id").unwrap())).collect::<Vec<_>>(), [("MidiTrack", "5"), ("MidiTrack", "8"), ("AudioTrack", "7")]);
        let (orig, copy) = (tracks[0], tracks[1]);
        // original: old instrument, muted, renamed, its automation untouched
        assert_eq!(val(orig, "Name/UserName"), Some("Bass (old VST2, muted)"));
        assert_eq!(val(orig, "DeviceChain/Mixer/Speaker/Manual"), Some("false"));
        assert!(orig.descendants().any(|n| tag(n) == "VstPluginInfo"));
        assert!(!orig.descendants().any(|n| tag(n) == "Vst3PluginInfo"));
        assert_eq!(val(orig, "AutomationEnvelopes/Envelopes/AutomationEnvelope/EnvelopeTarget/PointeeId"), Some("10"));
        // copy: VST3, playing, new name, fresh IDs, its envelope follows its own On target
        assert_eq!(val(copy, "Name/UserName"), Some("Bass (VST3)"));
        assert_eq!(val(copy, "DeviceChain/Mixer/Speaker/Manual"), Some("true"));
        assert!(copy.descendants().any(|n| tag(n) == "Vst3PluginInfo"));
        assert!(!copy.descendants().any(|n| tag(n) == "VstPluginInfo"));
        let on = find(copy, "DeviceChain/DeviceChain/Devices/PluginDevice/On/AutomationTarget").unwrap().attribute("Id").unwrap();
        assert_ne!(on, "10");
        assert_eq!(val(copy, "AutomationEnvelopes/Envelopes/AutomationEnvelope/EnvelopeTarget/PointeeId"), Some(on));
        // every automation target Id unique, NextPointeeId above them all
        let ids: Vec<i64> = doc.descendants().filter(|n| tag(*n) == "AutomationTarget").map(|n| n.attribute("Id").unwrap().parse().unwrap()).collect();
        assert_eq!(ids.len(), ids.iter().collect::<HashSet<_>>().len());
        let next: i64 = val(find(doc.root_element(), "LiveSet").unwrap(), "NextPointeeId").unwrap().parse().unwrap();
        assert!(ids.iter().all(|i| *i < next));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn valhalla() -> Vec<Plugin> {
        let mut uid = [0u8; 16];
        uid[..7].copy_from_slice(b"VSTRuum");
        uid[7..].copy_from_slice(b"valhallar");
        vec![Plugin { uid, name: "ValhallaRoom".into(), vendor: "Valhalla DSP, LLC".into(), kind: "audiofx".into(), dev_id: "device:vst3:audiofx:x".into() }]
    }

    /// Live 9 and Live 10.0 had no VST3: converting must refuse instead of
    /// writing a set Live calls corrupt.
    #[test]
    fn sets_from_before_live_10_1_are_not_converted() {
        let dir = std::env::temp_dir().join(format!("lv-fmt9-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (major, minor, creator) in [("4", "9.5_327", "Ableton Live 9.7.4"), ("5", "10.0_370", "Ableton Live 10.0.5b1")] {
            let p = tiny_set(&dir, "Old Song v_12.als", major, minor, creator);
            let plan = analyze(&p, Some(valhalla())).unwrap();
            assert_eq!(plan.items.len(), 1);
            assert_eq!(plan.items[0].mode, Mode::Vst2State, "the plug-in itself is convertible");
            assert!(plan.blocked.as_deref().unwrap_or("").contains(creator), "{creator}");
            assert!(convert(&plan, Some(dir.join("out.als"))).is_err(), "{creator}");
            assert!(!dir.join("out.als").exists(), "nothing may be written");
            assert!(preview_report(&plan)[0].starts_with("Can't convert"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// VST3 entries get exactly the optional fields the set's Live version writes.
    #[test]
    fn vst3_entry_matches_the_sets_live_version() {
        let dir = std::env::temp_dir().join(format!("lv-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (minor, creator, bus, placeholder) in [
            ("11.0_436", "Ableton Live 11.1.6", false, false),
            ("11.0_11300", "Ableton Live 11.3.13", true, false),
            ("12.0_12402", "Ableton Live 12.4.16", true, true),
        ] {
            let p = tiny_set(&dir, "Song.als", "5", minor, creator);
            let plan = analyze(&p, Some(valhalla())).unwrap();
            assert!(plan.blocked.is_none());
            let out = dir.join("Song v_2.als");
            convert(&plan, Some(out.clone())).unwrap();
            let xml = als::read_xml(&out).unwrap();
            als::parse(&xml).unwrap();
            assert!(xml.contains("<Vst3PluginInfo"), "{creator}");
            assert_eq!(xml.contains("<NumAudioInputs"), bus, "{creator}");
            assert_eq!(xml.contains("<IsPlaceholderDevice"), placeholder, "{creator}");
            std::fs::remove_file(&out).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// VST3 entries Live already wrote in the set win over the version table.
    #[test]
    fn layout_mirrors_existing_vst3_entries() {
        let xml = r#"<Ableton MajorVersion="5" MinorVersion="12.0_12402"><LiveSet><Tracks><AudioTrack Id="1"><DeviceChain><DeviceChain><Devices>
            <PluginDevice Id="0"><PluginDesc><Vst3PluginInfo Id="0"><WinPosX Value="0" /><NumAudioInputs Value="1" /><Name Value="X" /></Vst3PluginInfo></PluginDesc>
            <ParameterList><PluginFloatParameter Id="0"><ParameterName Value="a" /><ParameterId Value="1" /></PluginFloatParameter></ParameterList></PluginDevice>
            </Devices></DeviceChain></DeviceChain></AudioTrack></Tracks></LiveSet></Ableton>"#;
        let doc = als::parse(xml).unwrap();
        let start = Vst3Layout { bus_counts: true, placeholder: true, flank: true };
        assert_eq!(mirror_layout(&doc, start), Vst3Layout { bus_counts: true, placeholder: false, flank: false });
    }

    /// Reads this machine's Live plug-in database if there is one (read-only).
    #[test]
    fn reads_live_plugin_database() {
        if live_plugin_dbs().is_empty() {
            return;
        }
        let cat = load_catalog();
        assert!(!cat.is_empty(), "no VST3s read from Live's database");
        assert!(cat.iter().all(|p| p.dev_id.starts_with("device:vst3:")));
    }
}
