//! Version naming ("Song v_7.als"), finding sibling versions, and the notes files.
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{als, changes, config};

pub const NOTES_MARKER: &str = "--- Your notes (kept when this file is regenerated) ---";

fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "<base><sep>v_<n>" where sep is a space, underscore or dash (or nothing).
    RE.get_or_init(|| Regex::new(r"(?i)^(?P<base>.*?)(?P<sep>[ _-]?)v_(?P<num>\d+)$").unwrap())
}

#[derive(Clone, Debug)]
pub struct Version {
    pub path: PathBuf,
    pub base: String,
    /// An unnumbered set counts as version 1.
    pub num: u32,
    pub sep: String,
    pub numbered: bool,
}

impl Version {
    pub fn label(&self) -> String {
        if self.numbered { format!("v_{}", self.num) } else { "original".into() }
    }
}

pub fn parse(path: &Path) -> Version {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(c) = version_re().captures(&stem) {
        return Version {
            path: path.to_path_buf(),
            base: c["base"].to_string(),
            num: c["num"].parse().unwrap_or(1),
            sep: c["sep"].to_string(),
            numbered: true,
        };
    }
    Version { path: path.to_path_buf(), base: stem, num: 1, sep: " ".into(), numbered: false }
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

/// All versions of the same set in its folder, oldest first.
pub fn family(path: &Path) -> Vec<Version> {
    let me = parse(path);
    let folder = path.parent().unwrap_or(Path::new("."));
    let key = me.base.trim().to_lowercase();
    let mut out: Vec<Version> = als_files(folder).iter().map(|p| parse(p)).filter(|v| v.base.trim().to_lowercase() == key).collect();
    out.sort_by(|a, b| a.num.cmp(&b.num).then(mtime(&a.path).cmp(&mtime(&b.path))));
    out
}

/// {base name: versions} for every set in a project folder.
pub fn families_in(folder: &Path) -> Vec<(String, Vec<Version>)> {
    let mut map: std::collections::BTreeMap<String, Vec<Version>> = Default::default();
    for p in als_files(folder) {
        let v = parse(&p);
        map.entry(v.base.trim().to_string()).or_default().push(v);
    }
    let mut out: Vec<(String, Vec<Version>)> = map.into_iter().collect();
    for (_, vs) in out.iter_mut() {
        vs.sort_by_key(|v| v.num);
    }
    out
}

/// Next free number after the highest existing one, so saving from an old
/// version (v_3 while v_6 exists) creates v_7 instead of overwriting v_4.
pub fn next_version_path(path: &Path) -> PathBuf {
    let me = parse(path);
    let top = family(path).iter().map(|v| v.num).max().unwrap_or(1).max(me.num);
    let sep = if me.numbered { me.sep.clone() } else { " ".into() };
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut n = top + 1;
    loop {
        let candidate = folder.join(format!("{}{}v_{}.als", me.base, sep, n));
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

    #[test]
    fn version_names() {
        let v = parse(Path::new("C:/x/2026-11-13 v_7.als"));
        assert_eq!((v.base.as_str(), v.num, v.sep.as_str(), v.numbered), ("2026-11-13", 7, " ", true));
        let v = parse(Path::new("C:/x/Song.als"));
        assert_eq!((v.base.as_str(), v.num, v.numbered), ("Song", 1, false));
        let v = parse(Path::new("C:/x/Song_v_12.als"));
        assert_eq!((v.base.as_str(), v.num, v.sep.as_str()), ("Song", 12, "_"));
    }

    #[test]
    fn next_version_skips_to_the_top() {
        let d = std::env::temp_dir().join(format!("lv-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for f in ["Song.als", "Song v_2.als", "Song v_3.als", "Song v_6.als", "Other v_9.als"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        assert_eq!(next_version_path(&d.join("Song v_3.als")).file_name().unwrap(), "Song v_7.als");
        assert_eq!(next_version_path(&d.join("Song.als")).file_name().unwrap(), "Song v_7.als");
        let _ = std::fs::remove_dir_all(&d);
        let d2 = std::env::temp_dir().join(format!("lv-test2-{}", std::process::id()));
        std::fs::create_dir_all(&d2).unwrap();
        std::fs::write(d2.join("Fresh.als"), b"x").unwrap();
        assert_eq!(next_version_path(&d2.join("Fresh.als")).file_name().unwrap(), "Fresh v_2.als");
        let _ = std::fs::remove_dir_all(&d2);
    }
}
