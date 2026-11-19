//! "Live Sets": the latest version of every set you've opened in Live, or of
//! every set in chosen folders. One entry per set (version family), showing
//! its most recently saved version.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{config, versions};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SetEntry {
    /// The most recently saved version.
    pub latest: PathBuf,
    /// The set's name without its version ("Song" for "Song v_7").
    pub base: String,
    /// The latest version's label ("v_7", "v054 MASTER", "—" when unnumbered).
    pub label: String,
    /// How many versions are in its folder.
    pub versions: usize,
    /// When the latest version was saved (seconds since 1970).
    pub saved: i64,
}

impl SetEntry {
    pub fn folder(&self) -> &Path {
        self.latest.parent().unwrap_or(Path::new(""))
    }
    /// Identifies the set (all its versions): folder + name, case-insensitive.
    pub fn key(&self) -> String {
        family_key(self.folder(), &self.base)
    }
}

/// The [`SetEntry::key`] of the set `path` is a version of.
pub fn key_for(path: &Path) -> String {
    family_key(path.parent().unwrap_or(Path::new("")), &versions::parse(path).base)
}

fn family_key(folder: &Path, base: &str) -> String {
    format!("{}|{}", folder.to_string_lossy().to_lowercase().replace('\\', "/"), base.trim().to_lowercase())
}

fn secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).map(secs).unwrap_or(0)
}

/// Live's backups ("Project/Backup/Song [2026-11-13 101500].als") aren't sets of their own.
fn in_backup_folder(p: &Path) -> bool {
    p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().eq_ignore_ascii_case("Backup")).unwrap_or(false)
}

/// Newest first; then by name.
fn sort(v: &mut [SetEntry]) {
    v.sort_by(|a, b| b.saved.cmp(&a.saved).then_with(|| a.base.to_lowercase().cmp(&b.base.to_lowercase())));
}

fn entry_for(base: &str, vs: &[versions::Version]) -> Option<SetEntry> {
    let latest = vs.iter().max_by_key(|v| (mtime(&v.path), v.num))?;
    Some(SetEntry {
        latest: latest.path.clone(),
        base: base.trim().to_string(),
        label: if latest.numbered() { latest.label() } else { "—".into() },
        versions: vs.len(),
        saved: mtime(&latest.path),
    })
}

/// One entry per set among `paths`: each path's family, looked up in its folder,
/// with that family's most recently saved version.
pub fn latest_of(paths: impl IntoIterator<Item = PathBuf>, words: &[String]) -> Vec<SetEntry> {
    let mut folders: HashMap<PathBuf, Vec<(String, Vec<versions::Version>)>> = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let Some(folder) = p.parent().map(Path::to_path_buf) else { continue };
        if in_backup_folder(&p) {
            continue;
        }
        let base = versions::parse_with(&p, words).base.trim().to_lowercase();
        if !seen.insert(family_key(&folder, &base)) {
            continue;
        }
        let fams = folders.entry(folder.clone()).or_insert_with(|| versions::families_in_with(&folder, words));
        if let Some((b, vs)) = fams.iter().find(|(b, _)| b.trim().to_lowercase() == base) {
            out.extend(entry_for(b, vs));
        }
    }
    sort(&mut out);
    out
}

/// One entry per folder (Live project): its most recently saved set, with how
/// many sets the folder has. `list` must be newest first, as everything here returns.
pub fn one_per_folder<'a>(list: impl IntoIterator<Item = &'a SetEntry>) -> Vec<(&'a SetEntry, usize)> {
    let mut out: Vec<(&SetEntry, usize)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for e in list {
        let k = e.folder().to_string_lossy().to_lowercase();
        match index.get(&k) {
            Some(&i) => out[i].1 += 1,
            None => {
                index.insert(k, out.len());
                out.push((e, 1));
            }
        }
    }
    out
}

// ----------------------------------------------------------------------------- recently opened

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct RecentFile {
    sets: Vec<RecentItem>,
    /// Sets removed from the list (family keys); opening one again brings it back.
    hidden: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RecentItem {
    path: PathBuf,
    /// When it was last seen open in Live (seconds since 1970).
    opened: i64,
}

const MAX_RECENT: usize = 2000;

fn recent_file() -> PathBuf {
    config::app_dir().join("recent_sets.json")
}

fn load_recent() -> RecentFile {
    std::fs::read_to_string(recent_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_recent(r: &RecentFile) {
    if let Ok(s) = serde_json::to_string_pretty(r) {
        let tmp = recent_file().with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, recent_file());
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Remember that `path` is open in Live.
pub fn record_opened(path: &Path) {
    let mut r = load_recent();
    let now = secs(SystemTime::now());
    match r.sets.iter_mut().find(|i| same_path(&i.path, path)) {
        Some(i) => i.opened = now,
        None => r.sets.push(RecentItem { path: path.to_path_buf(), opened: now }),
    }
    let words = versions::words(&config::load());
    let key = family_key(path.parent().unwrap_or(Path::new("")), &versions::parse_with(path, &words).base);
    r.hidden.retain(|h| *h != key);
    if r.sets.len() > MAX_RECENT {
        r.sets.sort_by_key(|i| std::cmp::Reverse(i.opened));
        r.sets.truncate(MAX_RECENT);
    }
    save_recent(&r);
}

/// Take a set off the recently-opened list (until it's opened again).
pub fn forget(e: &SetEntry) {
    let mut r = load_recent();
    let key = e.key();
    if !r.hidden.contains(&key) {
        r.hidden.push(key);
    }
    save_recent(&r);
}

/// The latest version of every set opened in Live: the ones this app has seen,
/// plus Live's own recent-documents list.
pub fn recent() -> Vec<SetEntry> {
    let r = load_recent();
    let words = versions::words(&config::load());
    let mut paths: Vec<PathBuf> = r.sets.iter().map(|i| i.path.clone()).collect();
    paths.extend(live_recent_paths());
    latest_of(paths, &words).into_iter().filter(|e| !r.hidden.contains(&e.key())).collect()
}

// ----------------------------------------------------------------------------- Live's own lists

/// Live's preference folders, most recently used first.
pub fn live_pref_dirs() -> Vec<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Ableton"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Preferences/Ableton"))
    };
    let mut dirs: Vec<PathBuf> = base
        .and_then(|b| std::fs::read_dir(b).ok())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("Live ")).unwrap_or(false))
                .map(|p| if cfg!(windows) { p.join("Preferences") } else { p })
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort_by_key(|d| std::cmp::Reverse(std::fs::metadata(d).and_then(|m| m.modified()).ok()));
    dirs
}

/// .als paths stored as UTF-16 text in one of Live's binary .cfg files (at either
/// byte alignment), with forward slashes turned into the system's separators.
pub fn als_paths_in_cfg(data: &[u8]) -> Vec<PathBuf> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(if cfg!(windows) { r"[A-Za-z]:[/\\][^\x00-\x1f]*?\.als" } else { r"/(?:Users|Volumes)/[^\x00-\x1f]*?\.als" }).unwrap()
    });
    let mut out = Vec::new();
    for off in 0..2 {
        if data.len() <= off {
            break;
        }
        let u16s: Vec<u16> = data[off..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16_lossy(&u16s);
        for m in re.find_iter(&text) {
            let s = m.as_str();
            out.push(PathBuf::from(if cfg!(windows) { s.replace('/', "\\") } else { s.to_string() }));
        }
    }
    out
}

/// Live's recently opened documents (from Preferences.cfg of each Live version).
pub fn live_recent_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in live_pref_dirs() {
        let Ok(data) = std::fs::read(d.join("Preferences.cfg")) else { continue };
        let prefix = d.to_string_lossy().to_lowercase();
        out.extend(als_paths_in_cfg(&data).into_iter().filter(|p| !p.to_string_lossy().to_lowercase().starts_with(&prefix) && p.is_file()));
    }
    out
}

// ----------------------------------------------------------------------------- folder scan

/// Folders that never hold sets of their own (or hold thousands of samples).
fn skip_dir(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with('.')
        || n.starts_with('$')
        || matches!(n.as_str(), "backup" | "samples" | "ableton project info" | "version notes" | "system volume information" | "node_modules")
}

const MAX_DEPTH: usize = 16;

/// The latest version of every set in `roots` and their subfolders, newest first.
/// `progress` gets the number of folders looked at so far.
pub fn scan(roots: &[PathBuf], cancel: &AtomicBool, mut progress: impl FnMut(usize)) -> Vec<SetEntry> {
    let words = versions::words(&config::load());
    let mut out = Vec::new();
    let mut seen_dirs = std::collections::HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    let mut n = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !seen_dirs.insert(dir.to_string_lossy().to_lowercase()) {
            continue;
        }
        n += 1;
        if n % 50 == 0 {
            progress(n);
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut has_sets = false;
        for e in rd.flatten() {
            // Not following links: file_type() is the link itself.
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if depth < MAX_DEPTH && !skip_dir(&name) {
                    stack.push((e.path(), depth + 1));
                }
            } else if ft.is_file() && name.to_lowercase().ends_with(".als") {
                has_sets = true;
            }
        }
        if has_sets {
            for (b, vs) in versions::families_in_with(&dir, &words) {
                out.extend(entry_for(&b, &vs));
            }
        }
    }
    progress(n);
    sort(&mut out);
    out
}

fn scan_cache_file() -> PathBuf {
    config::cache_dir().join("set_scan.json")
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct ScanCache {
    roots: Vec<PathBuf>,
    sets: Vec<SetEntry>,
}

/// The last scan of exactly these folders, to show while scanning again.
pub fn cached_scan(roots: &[PathBuf]) -> Option<Vec<SetEntry>> {
    let c: ScanCache = std::fs::read_to_string(scan_cache_file()).ok().and_then(|s| serde_json::from_str(&s).ok())?;
    (c.roots == roots).then_some(c.sets)
}

pub fn save_scan(roots: &[PathBuf], sets: &[SetEntry]) {
    let c = ScanCache { roots: roots.to_vec(), sets: sets.to_vec() };
    if let Ok(s) = serde_json::to_string(&c) {
        let _ = std::fs::write(scan_cache_file(), s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, age_secs: u64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
        let t = SystemTime::now() - std::time::Duration::from_secs(age_secs);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    fn words() -> Vec<String> {
        versions::words(&config::Config::default())
    }

    #[test]
    fn one_entry_per_set_with_its_latest_version() {
        let root = std::env::temp_dir().join(format!("lv-sets-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = root.join("Song Project");
        touch(&a.join("Song.als"), 500);
        touch(&a.join("Song v_2.als"), 400);
        touch(&a.join("Song v_3.als"), 300);
        // An older number saved last (e.g. worked on again) is the latest saved.
        touch(&a.join("Song v054 MASTER.als"), 100);
        touch(&a.join("Other Edit.als"), 200);
        touch(&a.join("Backup").join("Song [2026-11-13 101500].als"), 10);
        touch(&a.join("Samples").join("Recorded").join("Take.als"), 5);
        let b = root.join("Old").join("Deep Project");
        touch(&b.join("Deep T01.als"), 900);
        touch(&b.join("Deep T02.als"), 800);

        let found = scan(&[root.clone()], &AtomicBool::new(false), |_| {});
        let names: Vec<(&str, &str, usize)> = found.iter().map(|e| (e.base.as_str(), e.label.as_str(), e.versions)).collect();
        assert_eq!(names, [("Song", "v054 MASTER", 4), ("Other Edit", "—", 1), ("Deep", "T02", 2)]);

        // From opened paths: any version of a set gives the same single entry.
        let opened = vec![a.join("Song v_2.als"), a.join("Song.als"), b.join("Deep T01.als"), a.join("Backup").join("Song [2026-11-13 101500].als")];
        let rec = latest_of(opened, &words());
        assert_eq!(rec.iter().map(|e| e.latest.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), ["Song v054 MASTER.als", "Deep T02.als"]);
        assert_eq!(rec[0].key(), found[0].key());

        // One per project folder: the newest set there, and how many sets it has.
        let per: Vec<(&str, usize)> = one_per_folder(&found).into_iter().map(|(e, n)| (e.base.as_str(), n)).collect();
        assert_eq!(per, [("Song", 2), ("Deep", 1)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paths_from_live_cfg() {
        // UTF-16 strings at odd and even offsets, as in Preferences.cfg.
        let mut data = vec![0u8, 7];
        let p1 = if cfg!(windows) { "D:/Music/Song Project/Song v_3.als" } else { "/Users/me/Music/Song Project/Song v_3.als" };
        let p2 = if cfg!(windows) { "C:/Temp/x.als" } else { "/Volumes/Disk/x.als" };
        for u in p1.encode_utf16() {
            data.extend(u.to_le_bytes());
        }
        data.extend([1, 0, 0]);
        for u in p2.encode_utf16() {
            data.extend(u.to_le_bytes());
        }
        let got = als_paths_in_cfg(&data);
        let want: Vec<PathBuf> = [p1, p2].iter().map(|p| PathBuf::from(if cfg!(windows) { p.replace('/', "\\") } else { p.to_string() })).collect();
        assert_eq!(got, want);
    }
}
