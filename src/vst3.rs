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
    SideBySide,
    Default,
    Skip,
}

impl Mode {
    pub fn long(self) -> &'static str {
        match self {
            Mode::Vst2State => "Keep settings (VST3 accepts the VST2 data)",
            Mode::Raw => "Keep settings: pass data as-is (try it)",
            Mode::SideBySide => "Default settings + keep old plug-in (off) to copy settings",
            Mode::Default => "VST3 with default settings",
            Mode::Skip => "Don't convert",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Mode::Vst2State => "Settings kept (VST2-compatible VST3)",
            Mode::Raw => "Old data passed as-is — check in Live",
            Mode::SideBySide => "Defaults + old plug-in kept (off)",
            Mode::Default => "Default settings",
            Mode::Skip => "Not converted",
        }
    }
    pub fn from_name(s: &str) -> Option<Mode> {
        Some(match s {
            "vst2state" => Mode::Vst2State,
            "raw" => Mode::Raw,
            "sidebyside" => Mode::SideBySide,
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

fn default_mode(src: &Source, m: Option<&str>) -> (Mode, &'static str) {
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
}

impl Item {
    pub fn old_label(&self) -> String {
        format!("{} [{}]", self.src.name, self.src.fmt)
    }
    pub fn new_label(&self) -> String {
        self.target.as_ref().map(|t| format!("{} [VST3]", t.name)).unwrap_or_else(|| "— no VST3 found —".into())
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
        m.extend([Mode::SideBySide, Mode::Default, Mode::Skip]);
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
    let (items, envelopes, next_pointee, max_target_id) = {
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
            let track = dev
                .ancestors()
                .find(|a| a.is_element() && tag(*a).ends_with("Track"))
                .map(|t| val(t, "Name/EffectiveName").map(String::from).unwrap_or_else(|| tag(t).replace("Track", "")))
                .unwrap_or_default();
            let (target, match_kind) = find_target(&src, &catalog);
            let (mode, note) = default_mode(&src, match_kind);
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
            });
        }
        (items, envelopes, next_pointee, max_target_id)
    };
    Ok(Plan { path: path.to_path_buf(), xml, items, catalog_size: catalog.len(), envelopes, next_pointee, max_target_id })
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

fn build_vst3_info(item: &Item, target: &Plugin, state: &[u8], nl: &str) -> String {
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
    s += &format!("{i1}<NumAudioInputs Value=\"{}\" />{nl}", esc(&inputs));
    s += &format!("{i1}<NumAudioOutputs Value=\"{}\" />{nl}", esc(&outputs));
    s += &format!("{i1}<IsPlaceholderDevice Value=\"false\" />{nl}");
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
    let what = match item.mode {
        Mode::Vst2State => "settings kept",
        Mode::Raw => "settings passed as-is — check it",
        Mode::SideBySide => "default settings; old plug-in kept next to it (off)",
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
        out.push(format!("      lost automation for: {shown} (VST3 parameter IDs not learned yet)"));
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
    let count = |m: Mode| plan.items.iter().filter(|i| i.target.is_some() && i.mode == m).count();
    let conv = n - plan.items.iter().filter(|i| i.target.is_none() || i.mode == Mode::Skip).count();
    let mut out = vec![format!("Plug-in report for {set}: {conv} of {n} VST2/AU plug-ins can be converted to VST3")];
    let parts: Vec<String> = [
        (Mode::Vst2State, "keeps its settings", "keep their settings"),
        (Mode::Raw, "passes its data as-is (check in Live)", "pass their data as-is (check in Live)"),
        (Mode::SideBySide, "starts from defaults with the old plug-in kept (off)", "start from defaults with the old plug-in kept (off)"),
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

        edits.push((item.info.clone(), build_vst3_info(item, target, &state_for(item), nl)));
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
                    if let (Some(f), false) = (flank, p.has_flank) {
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
