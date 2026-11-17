//! Talking to a running Ableton Live on Windows.
//!
//! * Which set is open: Live keeps the open document's full path in
//!   %APPDATA%\Ableton\Live x.y.z\Preferences\CrashRecoveryInfo.cfg (UTF-16), and
//!   its window title is "<set name>[*] - Ableton Live 12 Suite".
//! * Saving a new version: press Ctrl+Shift+S in Live, put the full new path in
//!   the Save dialog's file-name box and confirm.
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetDlgCtrlID, GetForegroundWindow, GetParent, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SendMessageW, SetForegroundWindow, BM_CLICK,
    WM_SETTEXT,
};

#[derive(Debug)]
pub struct LiveError(pub String);

const VK_CTRL: u16 = 0x11;
const VK_SHIFT: u16 = 0x10;
const VK_ALT: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_ENTER: u16 = 0x0D;

fn title_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?P<name>.*?)(?P<dirty>\*)?(?: \[[^\]]*\])? - Ableton Live\b").unwrap())
}

// ----------------------------------------------------------------------------- windows

pub fn window_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got as usize])
    }
}

fn class_name(h: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetClassNameW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

pub fn exe_of(pid: u32) -> String {
    unsafe {
        let Ok(hp) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(hp, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(hp);
        if ok { String::from_utf16_lossy(&buf[..size as usize]) } else { String::new() }
    }
}

fn is_live_exe(path: &str) -> bool {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_lowercase().starts_with("ableton live")).unwrap_or(false)
}

/// All visible top-level windows (handle, title).
pub fn top_windows() -> Vec<(HWND, String)> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let v = &mut *(l.0 as *mut Vec<HWND>);
        if IsWindowVisible(h).as_bool() {
            v.push(h);
        }
        BOOL(1)
    }
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v.into_iter().map(|h| (h, window_text(h))).collect()
}

/// Live's main window and its title.
pub fn live_main_window() -> Option<(HWND, String)> {
    top_windows().into_iter().find(|(h, t)| title_re().is_match(t) && is_live_exe(&exe_of(window_pid(*h))))
}

pub fn live_is_foreground() -> bool {
    let h = unsafe { GetForegroundWindow() };
    !h.0.is_null() && is_live_exe(&exe_of(window_pid(h)))
}

/// (set name, has unsaved changes) from Live's window title.
pub fn title_info(title: &str) -> Option<(String, bool)> {
    let c = title_re().captures(title)?;
    Some((c["name"].trim().to_string(), c.name("dirty").is_some()))
}

// ----------------------------------------------------------------------------- which set is open

fn pref_dirs() -> Vec<PathBuf> {
    let base = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Ableton"));
    let mut dirs: Vec<PathBuf> = base
        .and_then(|b| std::fs::read_dir(b).ok())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("Live ")).unwrap_or(false))
                .map(|p| p.join("Preferences"))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort_by_key(|d| std::cmp::Reverse(std::fs::metadata(d).and_then(|m| m.modified()).ok()));
    dirs
}

fn paths_in_crash_info(pref: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(pref.join("CrashRecoveryInfo.cfg")) else { return vec![] };
    let re = Regex::new(r"[A-Za-z]:[/\\][^\x00-\x1f]*?\.als").unwrap();
    let pref_l = pref.to_string_lossy().to_lowercase().replace('\\', "/");
    let mut out = Vec::new();
    for off in 0..2 {
        let u16s: Vec<u16> = data[off..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16_lossy(&u16s);
        for m in re.find_iter(&text) {
            let p = m.as_str().to_string();
            if !p.to_lowercase().replace('\\', "/").starts_with(&pref_l) {
                out.push(p);
            }
        }
    }
    out
}

fn paths_in_log(pref: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(pref.join("Log.txt")) else { return vec![] };
    let text = String::from_utf8_lossy(&bytes);
    let re = Regex::new(r#"Loading document "([^"]+\.als)""#).unwrap();
    let mut v: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect();
    v.reverse();
    v
}

/// Full path of the set open in Live, checked against the window title.
pub fn current_set_path() -> Option<PathBuf> {
    let (_, title) = live_main_window()?;
    let (name, _) = title_info(&title)?;
    current_set_path_named(&name)
}

pub fn current_set_path_named(name: &str) -> Option<PathBuf> {
    let dirs = pref_dirs();
    let mut cands: Vec<String> = dirs.iter().flat_map(|d| paths_in_crash_info(d)).collect();
    if let Some(d) = dirs.first() {
        cands.extend(paths_in_log(d));
    }
    cands
        .into_iter()
        .map(|p| PathBuf::from(p.replace('/', "\\")))
        .find(|p| p.file_stem().map(|s| s.to_string_lossy() == name).unwrap_or(false) && p.exists())
}

// ----------------------------------------------------------------------------- keyboard

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn press(vks: &[u16]) {
    let mut v: Vec<INPUT> = vks.iter().map(|k| key(*k, false)).collect();
    v.extend(vks.iter().rev().map(|k| key(*k, true)));
    send(&v);
}

pub fn type_text(text: &str) {
    let mut v = Vec::new();
    for u in text.encode_utf16() {
        for up in [false, true] {
            let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
            v.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: u, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            });
        }
    }
    send(&v);
}

/// The user still holds Ctrl+Alt from the hotkey; Live would see Ctrl+Alt+Shift+S.
/// Wait for release, then force key-ups as a fallback.
pub fn wait_modifiers_released() {
    let mods = [VK_CTRL, VK_ALT, VK_SHIFT, VK_LWIN, VK_RWIN];
    let end = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < end {
        if !mods.iter().any(|m| unsafe { GetAsyncKeyState(*m as i32) } as u16 & 0x8000 != 0) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    send(&mods.iter().map(|m| key(*m, true)).collect::<Vec<_>>());
}

// ----------------------------------------------------------------------------- Save As dialog

fn children(parent: HWND) -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        (*(l.0 as *mut Vec<HWND>)).push(h);
        BOOL(1)
    }
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(parent, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// The "File name" box of a common Save dialog: Edit inside ComboBox inside ComboBoxEx32.
fn filename_edit(dialog: HWND) -> Option<HWND> {
    children(dialog).into_iter().find(|h| unsafe {
        if class_name(*h) != "Edit" {
            return false;
        }
        let Ok(p) = GetParent(*h) else { return false };
        if class_name(p) != "ComboBox" {
            return false;
        }
        GetParent(p).map(|gp| class_name(gp) == "ComboBoxEx32").unwrap_or(false)
    })
}

fn ok_button(dialog: HWND) -> Option<HWND> {
    children(dialog)
        .into_iter()
        .find(|h| unsafe { class_name(*h) == "Button" && GetDlgCtrlID(*h) == 1 && IsWindowVisible(*h).as_bool() })
}

/// A new foreground window of Live's process: normally the common '#32770' Save
/// dialog; anything else is handled by typing into the focused field.
fn wait_for_dialog(pid: u32, main: HWND, timeout: Duration) -> Option<HWND> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let h = unsafe { GetForegroundWindow() };
        if !h.0.is_null() && h != main && window_pid(h) == pid {
            if class_name(h) == "#32770" {
                return Some(h);
            }
            std::thread::sleep(Duration::from_millis(400));
            if unsafe { GetForegroundWindow() } == h {
                return Some(h);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn fill_save_dialog(dialog: HWND, full_path: &str) {
    if let Some(edit) = filename_edit(dialog) {
        let mut w: Vec<u16> = full_path.encode_utf16().collect();
        w.push(0);
        unsafe {
            SendMessageW(edit, WM_SETTEXT, WPARAM(0), LPARAM(w.as_ptr() as isize));
        }
        std::thread::sleep(Duration::from_millis(50));
        if let Some(ok) = ok_button(dialog) {
            unsafe {
                let _ = PostMessageW(ok, BM_CLICK, WPARAM(0), LPARAM(0));
            }
            return;
        }
        unsafe {
            let _ = SetForegroundWindow(dialog);
        }
        press(&[VK_ENTER]);
        return;
    }
    // Unknown dialog layout: the file-name box has focus with its text selected.
    press(&[VK_CTRL, 0x41]);
    type_text(full_path);
    std::thread::sleep(Duration::from_millis(50));
    press(&[VK_ENTER]);
}

/// Until the file exists, is newer than `since`, and its size stopped changing.
fn wait_for_file(path: &Path, since: std::time::SystemTime, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    let (mut last, mut stable) = (u64::MAX, 0);
    let since = since - Duration::from_secs(2);
    while Instant::now() < end {
        if let Ok(md) = std::fs::metadata(path) {
            if md.modified().map(|m| m >= since).unwrap_or(false) {
                if md.len() == last && md.len() > 0 {
                    stable += 1;
                    if stable >= 3 && std::fs::File::open(path).is_ok() {
                        return true;
                    }
                } else {
                    stable = 0;
                }
                last = md.len();
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Drive Live's Save As to write `new_path`; Live then has the new file open.
/// `window` targets another app's main window (used for testing).
pub fn save_as(new_path: &Path, timeout: Duration, window: Option<HWND>) -> Result<(), LiveError> {
    let main = match window {
        Some(w) => w,
        None => live_main_window().ok_or_else(|| LiveError("Ableton Live isn't running.".into()))?.0,
    };
    let pid = window_pid(main);
    wait_modifiers_released();
    unsafe {
        if GetForegroundWindow() != main {
            let _ = SetForegroundWindow(main);
            std::thread::sleep(Duration::from_millis(150));
        }
        if GetForegroundWindow() != main {
            // Never send Ctrl+Shift+S to whatever else happens to be focused.
            return Err(LiveError("Live isn't the active window — click into Live and try again.".into()));
        }
    }
    let started = std::time::SystemTime::now();
    press(&[VK_CTRL, VK_SHIFT, 0x53]);
    let dialog = wait_for_dialog(pid, main, Duration::from_secs(6))
        .ok_or_else(|| LiveError("Live's Save dialog didn't open (is another Live dialog open?).".into()))?;
    std::thread::sleep(Duration::from_millis(150));
    fill_save_dialog(dialog, &new_path.to_string_lossy());
    if !wait_for_file(new_path, started, timeout) {
        let name = new_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(LiveError(format!("Live didn't write {name} — check Live for a message.")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives Notepad's Save As (the same common dialog Live uses on Windows).
    /// Takes keyboard focus for a moment, so it's opt-in: `build.ps1 test -- --ignored`.
    #[test]
    #[ignore]
    fn save_as_via_common_dialog() {
        let dir = std::env::temp_dir().join("liveversions-saveas-test");
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("Test Song v_3.txt");
        let _ = std::fs::remove_file(&target);
        let mut child = std::process::Command::new("notepad.exe").spawn().unwrap();
        let mut hwnd = None;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(100));
            hwnd = top_windows().into_iter().find(|(h, _)| window_pid(*h) == child.id()).map(|(h, _)| h);
            if hwnd.is_some() {
                break;
            }
        }
        let hwnd = hwnd.expect("notepad window");
        std::thread::sleep(Duration::from_millis(300));
        if unsafe { GetForegroundWindow() } == hwnd {
            type_text("hello from the test");
        }
        let r = save_as(&target, Duration::from_secs(10), Some(hwnd));
        std::thread::sleep(Duration::from_millis(300));
        let title = window_text(hwnd);
        let _ = child.kill();
        r.map_err(|e| e.0).unwrap();
        assert!(target.exists(), "file was not written");
        assert!(title.starts_with("Test Song v_3"), "window title after save: {title}");
    }

    #[test]
    fn title_parsing() {
        assert_eq!(title_info("2026-11-13 v_7* - Ableton Live 12 Suite"), Some(("2026-11-13 v_7".into(), true)));
        assert_eq!(title_info("Song [Song Project] - Ableton Live 11 Suite"), Some(("Song".into(), false)));
        assert_eq!(title_info("Untitled - Notepad"), None);
    }
}
