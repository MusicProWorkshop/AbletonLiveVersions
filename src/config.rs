//! User settings (JSON in the per-user app-data folder) and app paths.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Saves the next version (only while Ableton Live is the active window).
    pub hotkey_save_version: String,
    /// Opens the version browser (from anywhere).
    pub hotkey_open_browser: String,
    /// "subfolder" = <Project>/Version Notes/<set>.txt, "beside" = "<set> - changes.txt" next to the .als
    pub notes_location: String,
    pub notes_subfolder: String,
    /// Write notes for older versions that have none when the browser shows them.
    pub auto_write_missing_notes: bool,
    /// Seconds to wait for Live to finish writing a big set after Save As.
    pub save_timeout_seconds: u64,
    /// Default version name format: the text between the set's name and the
    /// number, then one "#" per digit. " v_#" -> "Song v_2", "_#" -> "Song_2",
    /// " v###" -> "Song v002", " T##" -> "Song T02".
    pub version_format: String,
    /// Keep the format a set (or its folder) already uses for new versions.
    pub keep_existing_format: bool,
    pub les_warning_shown: bool,
    /// Folders the Live Sets window scans for sets (and their subfolders).
    pub set_folders: Vec<String>,
    /// Live Sets shows one set per project folder (the most recently saved one).
    pub sets_one_per_project: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey_save_version: "ctrl+alt+s".into(),
            hotkey_open_browser: "ctrl+alt+v".into(),
            notes_location: "subfolder".into(),
            notes_subfolder: "Version Notes".into(),
            auto_write_missing_notes: true,
            save_timeout_seconds: 120,
            version_format: " v_#".into(),
            keep_existing_format: true,
            les_warning_shown: false,
            set_folders: Vec::new(),
            sets_one_per_project: true,
        }
    }
}

pub fn app_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
    }
    .unwrap_or_else(std::env::temp_dir);
    let p = base.join("LiveVersions");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn cache_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|b| PathBuf::from(b).join("LiveVersions"))
    } else {
        None
    }
    .unwrap_or_else(app_dir);
    let p = base.join("cache");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

pub fn load() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => {
            let c = Config::default();
            save(&c);
            c
        }
    }
}

pub fn save(c: &Config) {
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(config_path(), s);
    }
}

pub fn modified() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()).and_then(|m| m.modified()).ok()
}
