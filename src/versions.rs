//! Version naming ("Song v_7.als", "Song_7.als", "Song v007.als", "Song T07.als"…),
//! finding sibling versions, and the notes files.
use regex::Regex;
use std::path::{Path, PathBuf};

use crate::{als, changes, config};

pub const NOTES_MARKER: &str = "--- Your notes (kept when this file is regenerated) ---";

/// Words that mark a version number when they come right before it
/// ("v_3", "T01", "Take 2"). The word from the user's default format is added.
const VERSION_WORDS: &[&str] = &["v", "ver", "version", "rev", "r", "t", "take"];
const SEPARATORS: &[char] = &[' ', '_', '-', '.'];

/// How a version number is written after the set's name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VersionFormat {
    /// Everything between the name and the digits: " v_", "_", " v", " T", " Take ".
    pub marker: String,
    /// Minimum number of digits, zero-padded: 1 for "v_3", 3 for "v001".
    pub width: usize,
}

impl VersionFormat {
    pub fn render(&self, n: u32) -> String {
        format!("{}{:0w$}", self.marker, n, w = self.width)
    }

    /// From a settings pattern: the marker followed by one "#" per digit, e.g. " v_#", " T##", "_#".
    pub fn from_pattern(p: &str) -> Option<VersionFormat> {
        let width = p.chars().rev().take_while(|c| *c == '#').count();
        let marker = &p[..p.len() - width];
        (width > 0 && !marker.contains('#') && !marker.is_empty()).then(|| VersionFormat { marker: marker.to_string(), width })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pattern(&self) -> String {
        format!("{}{}", self.marker, "#".repeat(self.width))
    }

    /// The marker's word ("v", "T", "Take"), if any.
    fn word(&self) -> String {
        self.marker.chars().filter(|c| c.is_alphabetic()).collect()
    }
}

pub fn default_format() -> VersionFormat {
    VersionFormat { marker: " v_".into(), width: 1 }
}

/// The user's default format from the settings (falls back to " v_#").
pub fn configured_format(cfg: &config::Config) -> VersionFormat {
    VersionFormat::from_pattern(&cfg.version_format).unwrap_or_else(default_format)
}

#[derive(Clone, Debug)]
pub struct Version {
    pub path: PathBuf,
    pub base: String,
    /// An unnumbered set counts as version 1.
    pub num: u32,
    /// None for an unnumbered set ("Song.als").
    pub format: Option<VersionFormat>,
    /// The version part as written, e.g. "v_3", "_8", "T01".
    token: String,
}

impl Version {
    pub fn numbered(&self) -> bool {
        self.format.is_some()
    }
    pub fn label(&self) -> String {
        if self.numbered() { self.token.clone() } else { "original".into() }
    }
}

/// Recognised version words: the built-in ones plus the word of the user's default format.
pub fn words(cfg: &config::Config) -> Vec<String> {
    let mut w: Vec<String> = VERSION_WORDS.iter().map(|s| s.to_string()).collect();
    let mine = configured_format(cfg).word().to_lowercase();
    if !mine.is_empty() && !w.contains(&mine) {
        w.push(mine);
    }
    w
}

/// Split a set name into (base, format, number). Recognised endings:
/// "<sep?><word><sep?><digits>" with a version word ("v_3", " v3", " T01", " Take 2"),
/// and Live Enhancement Suite's "_<digits>". Dates ("2026-11-13") and names like
/// "Kick 2" are not versions.
pub fn split_version(stem: &str, words: &[String]) -> Option<(String, VersionFormat, u32, String)> {
    let digits_at = stem.len() - stem.chars().rev().take_while(|c| c.is_ascii_digit()).count();
    let digits = &stem[digits_at..];
    if digits.is_empty() || digits.len() > 6 || digits_at == 0 {
        return None;
    }
    let num: u32 = digits.parse().ok()?;
    let width = if digits.len() > 1 && digits.starts_with('0') { digits.len() } else { 1 };
    let rest = &stem[..digits_at];

    // "<word><sep?>": e.g. "Song v_" / "Song T" / "Song Take "
    let no_mid = rest.strip_suffix(SEPARATORS).unwrap_or(rest);
    let word_at = no_mid.len() - no_mid.chars().rev().take_while(|c| c.is_ascii_alphabetic()).count();
    let word = &no_mid[word_at..];
    if !word.is_empty() && words.iter().any(|w| w.eq_ignore_ascii_case(word)) {
        let before = &no_mid[..word_at];
        let base = before.strip_suffix(SEPARATORS).unwrap_or(before);
        // The word must stand on its own: "Song T01" yes, "SongT01" no.
        let glued = base.len() == before.len() && before.chars().last().map(|c| c.is_alphanumeric()).unwrap_or(false);
        if !base.trim().is_empty() && !glued {
            let marker = stem[base.len()..digits_at].to_string();
            let token = stem[base.len()..].trim_start_matches(SEPARATORS).to_string();
            return Some((base.to_string(), VersionFormat { marker, width }, num, token));
        }
    }

    // Live Enhancement Suite: "<name>_<digits>", but not the end of a date like "2022_03_19".
    if let Some(base) = rest.strip_suffix('_') {
        let date_like = Regex::new(r"\d{4}[-_.]\d{1,2}$").unwrap().is_match(base);
        if !base.trim().is_empty() && !base.ends_with(SEPARATORS) && !date_like {
            return Some((base.to_string(), VersionFormat { marker: "_".into(), width }, num, format!("_{digits}")));
        }
    }
    None
}

pub fn parse_with(path: &Path, words: &[String]) -> Version {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match split_version(&stem, words) {
        Some((base, format, num, token)) => Version { path: path.to_path_buf(), base, num, format: Some(format), token },
        None => Version { path: path.to_path_buf(), base: stem, num: 1, format: None, token: String::new() },
    }
}

pub fn parse(path: &Path) -> Version {
    parse_with(path, &words(&config::load()))
}

fn mtime(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

fn als_files(folder: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(folder) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("als")).unwrap_or(false) && p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

fn family_with(path: &Path, words: &[String]) -> Vec<Version> {
    let me = parse_with(path, words);
    let folder = path.parent().unwrap_or(Path::new("."));
    let key = me.base.trim().to_lowercase();
    let mut out: Vec<Version> =
        als_files(folder).iter().map(|p| parse_with(p, words)).filter(|v| v.base.trim().to_lowercase() == key).collect();
    out.sort_by(|a, b| a.num.cmp(&b.num).then(mtime(&a.path).cmp(&mtime(&b.path))));
    out
}

/// All versions of the same set in its folder, oldest first (whatever format each uses).
pub fn family(path: &Path) -> Vec<Version> {
    family_with(path, &words(&config::load()))
}

/// {base name: versions} for every set in a project folder.
pub fn families_in(folder: &Path) -> Vec<(String, Vec<Version>)> {
    let words = words(&config::load());
    let mut map: std::collections::BTreeMap<String, Vec<Version>> = Default::default();
    for p in als_files(folder) {
        let v = parse_with(&p, &words);
        map.entry(v.base.trim().to_string()).or_default().push(v);
    }
    let mut out: Vec<(String, Vec<Version>)> = map.into_iter().collect();
    for (_, vs) in out.iter_mut() {
        vs.sort_by_key(|v| v.num);
    }
    out
}

/// The format of the most recently saved numbered set in the folder (the one
/// you've been using lately, even if older sets there use another format).
fn folder_format(folder: &Path, words: &[String]) -> Option<VersionFormat> {
    als_files(folder)
        .into_iter()
        .filter_map(|p| parse_with(&p, words).format.map(|f| (mtime(&p), f)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, f)| f)
}

/// Which format the next version of `path` gets: the set's own format, else that
/// of its other versions, else the latest one used in the folder, else the default.
/// With "keep existing format" off, always the default.
pub fn format_for_next(path: &Path, cfg: &config::Config) -> VersionFormat {
    let default = configured_format(cfg);
    if !cfg.keep_existing_format {
        return default;
    }
    let words = words(cfg);
    let me = parse_with(path, &words);
    let folder = path.parent().unwrap_or(Path::new("."));
    me.format
        .or_else(|| {
            family_with(path, &words)
                .into_iter()
                .filter(|v| v.format.is_some())
                .max_by_key(|v| (v.num, mtime(&v.path)))
                .and_then(|v| v.format)
        })
        .or_else(|| folder_format(folder, &words))
        .unwrap_or(default)
}

/// Next free number after the highest existing one (in any format), so saving
/// from an old version (v_3 while v_6 exists) creates v_7 instead of overwriting v_4.
pub fn next_version_path(path: &Path) -> PathBuf {
    next_version_path_with(path, &config::load())
}

pub fn next_version_path_with(path: &Path, cfg: &config::Config) -> PathBuf {
    let words = words(cfg);
    let me = parse_with(path, &words);
    let top = family_with(path, &words).iter().map(|v| v.num).max().unwrap_or(1).max(me.num);
    let format = format_for_next(path, cfg);
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut n = top + 1;
    loop {
        let candidate = folder.join(format!("{}{}.als", me.base, format.render(n)));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// The version this one was saved from (recorded in its notes), else the closest lower number.
pub fn previous_version(path: &Path) -> Option<PathBuf> {
    if let Some(parent) = read_notes(path).parent {
        let p = path.parent().unwrap_or(Path::new(".")).join(parent);
        if p.exists() {
            return Some(p);
        }
    }
    let me = parse(path);
    family(path).into_iter().filter(|v| v.num < me.num).last().map(|v| v.path)
}

// ----------------------------------------------------------------------------- notes files

pub fn notes_path(als_path: &Path) -> PathBuf {
    let cfg = config::load();
    let folder = als_path.parent().unwrap_or(Path::new("."));
    let stem = als_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if cfg.notes_location == "beside" {
        folder.join(format!("{stem} - changes.txt"))
    } else {
        let sub = if cfg.notes_subfolder.trim().is_empty() { "Version Notes" } else { cfg.notes_subfolder.trim() };
        folder.join(sub).join(format!("{stem}.txt"))
    }
}

#[derive(Default, Clone, Debug)]
pub struct Notes {
    /// The generated part.
    pub text: String,
    /// What the user wrote.
    pub user: String,
    /// File name of the version this one was compared with.
    pub parent: Option<String>,
    /// Extra block, e.g. a VST3 conversion report (kept when regenerating).
    pub extra: Option<Vec<String>>,
    pub exists: bool,
}

pub fn read_notes(als_path: &Path) -> Notes {
    let Ok(content) = std::fs::read_to_string(notes_path(als_path)) else { return Notes::default() };
    let content = content.replace("\r\n", "\n");
    let (gen, user) = match content.find(NOTES_MARKER) {
        Some(i) => (&content[..i], &content[i + NOTES_MARKER.len()..]),
        None => (&content[..], ""),
    };
    let parent = Regex::new(r"(?m)^Compared with: (.+\.als)\s*$").unwrap().captures(gen).map(|c| c[1].to_string());
    let extra = Regex::new(r"(?ms)^Ableton Live[^\n]*\n\n(.*?)\n\nSummary:")
        .unwrap()
        .captures(gen)
        .map(|c| c[1].lines().map(String::from).collect());
    Notes {
        text: gen.trim_end().to_string(),
        user: user.trim_matches('\n').to_string(),
        parent,
        extra,
        exists: true,
    }
}

fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let t = if cfg!(windows) { text.replace('\n', "\r\n") } else { text.to_string() };
    std::fs::write(path, t)
}

/// (Re)generate the change notes for a version. Returns the summary line.
pub fn write_notes(als_path: &Path, parent: Option<&Path>, extra: Option<Vec<String>>, user_text: Option<String>) -> Result<String, String> {
    let old = read_notes(als_path);
    let extra = extra.or(old.extra.clone());
    let parent: Option<PathBuf> = parent.map(Path::to_path_buf).or_else(|| previous_version(als_path));
    let new_snap = als::snapshot(als_path)?;
    let old_snap = match &parent {
        Some(p) => Some(als::snapshot(p)?),
        None => None,
    };
    let saved: chrono::DateTime<chrono::Local> = mtime(als_path).into();
    let stem = als_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut lines = vec![stem.clone(), "=".repeat(stem.chars().count()), format!("Saved: {}", saved.format("%Y-%m-%d %H:%M"))];
    if let Some(p) = &parent {
        lines.push(format!("Compared with: {}", p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    lines.push(changes::set_overview(&new_snap));
    lines.push(String::new());
    if let Some(x) = &extra {
        lines.extend(x.iter().cloned());
        lines.push(String::new());
    }
    let summary;
    match &old_snap {
        None => {
            summary = format!("First version · {}", changes::counts_line(&new_snap));
            lines.push(format!("Summary: {summary}"));
            lines.push(String::new());
            lines.push("Contents:".into());
            lines.extend(changes::contents_lines(&new_snap));
        }
        Some(o) => {
            let ch = changes::diff(o, &new_snap);
            summary = changes::summary_line(&ch);
            lines.push(format!("Summary: {summary}"));
            lines.push(String::new());
            lines.extend(changes::format_lines(&ch));
        }
    }
    let user = user_text.unwrap_or(old.user);
    lines.extend([String::new(), String::new(), NOTES_MARKER.to_string(), user]);
    let text = lines.join("\n").trim_end().to_string() + "\n";
    write_text(&notes_path(als_path), &text).map_err(|e| format!("Can't write notes: {e}"))?;
    Ok(summary)
}

pub fn save_user_note(als_path: &Path, text: &str) -> Result<(), String> {
    let n = read_notes(als_path);
    if !n.exists {
        return write_notes(als_path, None, None, Some(text.to_string())).map(|_| ());
    }
    let body = format!("{}\n\n\n{}\n{}\n", n.text, NOTES_MARKER, text.trim());
    write_text(&notes_path(als_path), &body).map_err(|e| e.to_string())
}

pub fn summary_from_notes(n: &Notes) -> Option<String> {
    let re = Regex::new(r"(?m)^Summary: (.+)$").unwrap();
    if let Some(c) = re.captures(&n.text) {
        return Some(c[1].trim().to_string());
    }
    n.text.contains("First version").then(|| "First version".to_string())
}

pub fn tracks_from_notes(n: &Notes) -> Option<String> {
    Regex::new(r"· (\d+) tracks").unwrap().captures(&n.text).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(stem: &str) -> Option<(String, String, usize, u32, String)> {
        split_version(stem, &words(&config::Config::default())).map(|(b, f, n, t)| (b, f.marker, f.width, n, t))
    }

    fn some(base: &str, marker: &str, width: usize, n: u32, token: &str) -> Option<(String, String, usize, u32, String)> {
        Some((base.into(), marker.into(), width, n, token.into()))
    }

    #[test]
    fn recognizes_version_formats() {
        assert_eq!(split("2026-11-13 v_7"), some("2026-11-13", " v_", 1, 7, "v_7"));
        assert_eq!(split("Song_v_12"), some("Song", "_v_", 1, 12, "v_12"));
        assert_eq!(split("2022-03-19 Midi Melodies_8"), some("2022-03-19 Midi Melodies", "_", 1, 8, "_8")); // LES
        assert_eq!(split("Song v001"), some("Song", " v", 3, 1, "v001"));
        assert_eq!(split("Song T01"), some("Song", " T", 2, 1, "T01"));
        assert_eq!(split("Song V2"), some("Song", " V", 1, 2, "V2"));
        assert_eq!(split("Song Take 3"), some("Song", " Take ", 1, 3, "Take 3"));
        assert_eq!(split("Song v10"), some("Song", " v", 1, 10, "v10"));
        // not versions
        for stem in ["Song", "2026-11-13", "Kick 2", "SongT01", "2022_03_19", "Eighties & Teens v_7 Serum Brass", "v2", "_3"] {
            assert_eq!(split(stem), None, "{stem}");
        }
    }

    #[test]
    fn patterns() {
        let f = VersionFormat::from_pattern(" v###").unwrap();
        assert_eq!((f.render(7), f.render(1234)), (" v007".into(), " v1234".into()));
        assert_eq!(VersionFormat::from_pattern("_#").unwrap().render(12), "_12");
        assert_eq!(VersionFormat::from_pattern(" T##").unwrap().pattern(), " T##");
        for bad in ["#", "", " v", " v#x", " #v#"] {
            assert_eq!(VersionFormat::from_pattern(bad), None, "{bad:?}");
        }
    }

    /// A temp folder with empty .als files, oldest first (each 10 s newer than the last).
    fn folder(name: &str, files: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for (i, f) in files.iter().enumerate() {
            let file = std::fs::File::create(d.join(f)).unwrap();
            file.set_modified(t0 + std::time::Duration::from_secs(10 * i as u64)).unwrap();
        }
        d
    }

    fn next(d: &Path, from: &str, cfg: &config::Config) -> String {
        next_version_path_with(&d.join(from), cfg).file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn next_version_skips_to_the_top() {
        let cfg = config::Config::default();
        let d = folder("top", &["Song.als", "Song v_2.als", "Song v_3.als", "Song v_6.als", "Other v_9.als"]);
        assert_eq!(next(&d, "Song v_3.als", &cfg), "Song v_7.als");
        assert_eq!(next(&d, "Song.als", &cfg), "Song v_7.als");
        let _ = std::fs::remove_dir_all(&d);
        let d = folder("fresh", &["Fresh.als"]);
        assert_eq!(next(&d, "Fresh.als", &cfg), "Fresh v_2.als");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn keeps_the_existing_format() {
        let cfg = config::Config::default(); // default " v_#"
        let d = folder("keep", &["Song.als", "Song T01.als", "Song T02.als", "Melodies_8.als", "Pad v009.als"]);
        assert_eq!(next(&d, "Song T02.als", &cfg), "Song T03.als");
        assert_eq!(next(&d, "Song.als", &cfg), "Song T03.als", "unnumbered set: format of its versions");
        assert_eq!(next(&d, "Melodies_8.als", &cfg), "Melodies_9.als");
        assert_eq!(next(&d, "Pad v009.als", &cfg), "Pad v010.als");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_set_takes_the_latest_format_in_the_folder() {
        let cfg = config::Config::default();
        // Older sets use v_, the most recent one T##: the new set follows T##
        // even though v_ is used more often.
        let d = folder("latest", &["A v_2.als", "A v_3.als", "B v_4.als", "C T05.als", "Fresh.als"]);
        assert_eq!(next(&d, "Fresh.als", &cfg), "Fresh T02.als");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_format_setting() {
        let d = folder("default", &["Song T02.als", "Fresh.als"]);
        // Keep-existing off: always the default; the number still continues.
        let cfg = config::Config { version_format: " v###".into(), keep_existing_format: false, ..Default::default() };
        assert_eq!(next(&d, "Song T02.als", &cfg), "Song v003.als");
        let _ = std::fs::remove_dir_all(&d);
        // Nothing to go by: the default.
        let d = folder("default2", &["Fresh.als"]);
        let cfg = config::Config { version_format: "_#".into(), ..Default::default() };
        assert_eq!(next(&d, "Fresh.als", &cfg), "Fresh_2.als");
        // A custom word in the default format is recognised afterwards.
        let cfg = config::Config { version_format: " mix#".into(), ..Default::default() };
        assert_eq!(next(&d, "Fresh.als", &cfg), "Fresh mix2.als");
        assert!(split_version("Fresh mix2", &words(&cfg)).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }
}
