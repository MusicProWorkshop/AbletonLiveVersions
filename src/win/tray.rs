//! The background part: a tray icon (with menu), global hotkeys and notifications.
//! Windows (browser, converter, settings) run as separate processes of the same exe,
//! so the tray stays tiny and responsive.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{config, versions};

use super::{live, toast};

const WM_TRAY: u32 = WM_APP + 1;
const WM_WORKER: u32 = WM_APP + 2;
const HK_SAVE: i32 = 1;
const HK_BROWSE: i32 = 2;
const ID_BROWSE: usize = 10;
const ID_CONVERT: usize = 11;
const ID_SETTINGS: usize = 12;
const ID_STARTUP: usize = 13;
const ID_QUIT: usize = 14;
const ID_NOTES_HELP: usize = 15;
const ID_REPORT: usize = 16;

enum Event {
    Balloon { title: String, body: String, error: bool },
    Saving(String),
    Saved { title: String, body: String },
    Tip(String),
}

static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
static SAVING: AtomicBool = AtomicBool::new(false);
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);

struct State {
    hwnd: HWND,
    icon: HICON,
    cfg: config::Config,
    cfg_mtime: Option<std::time::SystemTime>,
    taskbar_created: u32,
    browser: Option<Child>,
    hotkey_errors: Vec<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn post(e: Event) {
    EVENTS.lock().unwrap().push_back(e);
    let h = HWND_MAIN.load(Ordering::SeqCst);
    unsafe {
        let _ = PostMessageW(super::isize_to_hwnd(h), WM_WORKER, WPARAM(0), LPARAM(0));
    }
}

fn wide<const N: usize>(s: &str) -> [u16; N] {
    let mut buf = [0u16; N];
    for (i, c) in s.encode_utf16().take(N - 1).enumerate() {
        buf[i] = c;
    }
    buf
}

// ----------------------------------------------------------------------------- hotkeys

pub fn parse_hotkey(spec: &str) -> Result<(u32, u32), String> {
    let (mut mods, mut vk) = (0u32, None);
    for p in spec.split('+').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()) {
        match p.as_str() {
            "alt" => mods |= 0x1,
            "ctrl" | "control" => mods |= 0x2,
            "shift" => mods |= 0x4,
            "win" => mods |= 0x8,
            k if k.len() > 1 && k.starts_with('f') && k[1..].parse::<u32>().map(|n| (1..=24).contains(&n)).unwrap_or(false) => {
                vk = Some(0x6F + k[1..].parse::<u32>().unwrap())
            }
            "space" => vk = Some(0x20),
            "insert" => vk = Some(0x2D),
            "home" => vk = Some(0x24),
            "end" => vk = Some(0x23),
            "pageup" => vk = Some(0x21),
            "pagedown" => vk = Some(0x22),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => vk = Some(k.to_ascii_uppercase().as_bytes()[0] as u32),
            other => return Err(format!("unknown key \"{other}\"")),
        }
    }
    let vk = vk.ok_or("no key given")?;
    if mods == 0 {
        return Err("needs at least one of Ctrl, Alt, Shift, Win".into());
    }
    Ok((mods, vk))
}

pub fn pretty_hotkey(spec: &str) -> String {
    spec.split('+')
        .map(|p| {
            let p = p.trim();
            match p.to_lowercase().as_str() {
                "ctrl" | "control" => "Ctrl".to_string(),
                "alt" => "Alt".into(),
                "shift" => "Shift".into(),
                "win" => "Win".into(),
                o => {
                    let mut c = o.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

fn register_hotkeys(st: &mut State) {
    unsafe {
        let _ = UnregisterHotKey(st.hwnd, HK_SAVE);
        let _ = UnregisterHotKey(st.hwnd, HK_BROWSE);
    }
    st.hotkey_errors.clear();
    for (id, spec) in [(HK_SAVE, st.cfg.hotkey_save_version.clone()), (HK_BROWSE, st.cfg.hotkey_open_browser.clone())] {
        match parse_hotkey(&spec) {
            Ok((mods, vk)) => unsafe {
                if RegisterHotKey(st.hwnd, id, HOT_KEY_MODIFIERS(mods | 0x4000), vk).is_err() {
                    st.hotkey_errors.push(format!("{} is used by another program", pretty_hotkey(&spec)));
                }
            },
            Err(e) => st.hotkey_errors.push(format!("{spec}: {e}")),
        }
    }
}

// ----------------------------------------------------------------------------- tray icon

fn icon_data(st: &State) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: st.hwnd,
        uID: 1,
        ..Default::default()
    }
}

fn tooltip(st: &State) -> String {
    format!(
        "Live Versions\n{} save new version\n{} version browser",
        pretty_hotkey(&st.cfg.hotkey_save_version),
        pretty_hotkey(&st.cfg.hotkey_open_browser)
    )
}

fn add_icon(st: &State) {
    let mut d = icon_data(st);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = st.icon;
    d.szTip = wide(&tooltip(st));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
    }
}

fn set_tip(st: &State, tip: &str) {
    let mut d = icon_data(st);
    d.uFlags = NIF_TIP | NIF_SHOWTIP;
    d.szTip = wide(tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

fn balloon(_st: &State, title: &str, body: &str, error: bool) {
    let (kind, ms) = if error { (toast::Kind::Error, 9000) } else { (toast::Kind::Info, 6000) };
    toast::show(title, body, kind, ms);
}

/// Shows the tray menu and returns the chosen command. Runs a modal loop, so it
/// must be called without holding STATE (messages keep arriving meanwhile).
fn show_menu(hwnd: HWND, cfg: &config::Config) -> usize {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        let item = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
        };
        item(MF_STRING | MF_GRAYED, 0, "Live Versions is running");
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING, ID_BROWSE, &format!("Open version browser\t{}", pretty_hotkey(&cfg.hotkey_open_browser)));
        item(MF_STRING, ID_REPORT, "Plug-in report for a set…");
        item(MF_STRING, ID_CONVERT, "Convert a set to VST3…");
        item(MF_STRING, ID_NOTES_HELP, &format!("How to save a version: {} in Live", pretty_hotkey(&cfg.hotkey_save_version)));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING, ID_SETTINGS, "Settings…");
        item(MF_STRING | if super::startup_enabled() { MF_CHECKED } else { MF_UNCHECKED }, ID_STARTUP, "Start with Windows");
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING, ID_QUIT, "Quit Live Versions");
        let _ = SetMenuDefaultItem(m, ID_BROWSE as u32, 0);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        cmd.0 as usize
    }
}

// ----------------------------------------------------------------------------- actions

fn spawn_self(args: &[&str]) -> Option<Child> {
    super::allow_foreground();
    let exe = std::env::current_exe().ok()?;
    Command::new(exe).args(args).spawn().ok()
}

fn open_browser(st: &mut State) {
    if let Some(child) = st.browser.as_mut() {
        if matches!(child.try_wait(), Ok(None)) && super::focus_process(child.id()) {
            return;
        }
    }
    let path = live::current_set_path();
    let p = path.as_ref().map(|p| p.to_string_lossy().into_owned());
    let mut args = vec!["--browse"];
    if let Some(p) = &p {
        args.push(p);
    }
    st.browser = spawn_self(&args);
}

fn handle_command(st: &mut State, id: usize) {
    match id {
        ID_BROWSE => open_browser(st),
        ID_CONVERT => {
            spawn_self(&["--convert"]);
        }
        ID_REPORT => {
            spawn_self(&["--report"]);
        }
        ID_SETTINGS => {
            spawn_self(&["--settings"]);
        }
        ID_NOTES_HELP => balloon(
            st,
            "Saving a version",
            &format!(
                "In Live, press {}. The set is saved as the next version (v_2, v_3…) and a change note is written next to it.",
                pretty_hotkey(&st.cfg.hotkey_save_version)
            ),
            false,
        ),
        ID_STARTUP => {
            let on = !super::startup_enabled();
            if !super::set_startup(on) {
                balloon(st, "Couldn't change the startup setting", "Windows refused to update it.", true);
            }
        }
        ID_QUIT => unsafe {
            // Posted, not called: WM_DESTROY needs STATE, which we're holding here.
            let _ = PostMessageW(st.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        _ => {}
    }
}

fn start_save(st: &State) {
    if SAVING.swap(true, Ordering::SeqCst) {
        return;
    }
    let name = live::live_main_window().and_then(|(_, t)| live::title_info(&t)).map(|(n, _)| n);
    let timeout = Duration::from_secs(st.cfg.save_timeout_seconds.max(10));
    std::thread::spawn(move || {
        save_worker(name, timeout);
        SAVING.store(false, Ordering::SeqCst);
        post(Event::Tip(String::new()));
    });
}

fn save_worker(name: Option<String>, timeout: Duration) {
    let fail = |title: &str, body: String| post(Event::Balloon { title: title.into(), body, error: true });
    let Some(name) = name.filter(|n| !n.to_lowercase().starts_with("untitled")) else {
        return fail("Save the set first", "This set has never been saved. Save it once with Ctrl+S, then use the version shortcut.".into());
    };
    let Some(cur) = live::current_set_path_named(&name) else {
        return fail("Couldn't find this set on disk", format!("Live has \u{201c}{name}\u{201d} open but its file wasn't found. Save it once with Ctrl+S and try again."));
    };
    let new = versions::next_version_path(&cur);
    let stem = new.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    post(Event::Tip(format!("Live Versions\nSaving {stem}…")));
    post(Event::Saving(format!("Saving {stem}…")));
    if let Err(e) = live::save_as(&new, timeout, None) {
        return fail("Version not saved", e.0);
    }
    let summary = versions::write_notes(&new, Some(&cur), None, None).unwrap_or_else(|e| format!("(change notes failed: {e})"));
    post(Event::Saved { title: format!("Saved {stem}"), body: summary });
}

// ----------------------------------------------------------------------------- window procedure

/// Run `f` with the tray state. Returns None (and skips `f`) if the state is
/// already in use further up the stack, instead of panicking.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let tb = with_state(|s| s.taskbar_created).unwrap_or(0);
    match msg {
        WM_TRAY => {
            let ev = (lp.0 & 0xFFFF) as u32;
            if ev == WM_LBUTTONUP {
                with_state(open_browser);
            } else if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
                if let Some((h, cfg)) = with_state(|s| (s.hwnd, s.cfg.clone())) {
                    let cmd = show_menu(h, &cfg);
                    if cmd != 0 {
                        with_state(|s| handle_command(s, cmd));
                    }
                }
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            match wp.0 as i32 {
                HK_SAVE if live::live_is_foreground() => {
                    with_state(|s| start_save(s));
                }
                HK_BROWSE => {
                    with_state(open_browser);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            // Settings changed from the settings window: reload and re-register hotkeys.
            with_state(|s| {
                let m = config::modified();
                if m != s.cfg_mtime {
                    s.cfg_mtime = m;
                    let new = config::load();
                    let keys_changed = new.hotkey_save_version != s.cfg.hotkey_save_version || new.hotkey_open_browser != s.cfg.hotkey_open_browser;
                    s.cfg = new;
                    if keys_changed {
                        register_hotkeys(s);
                        if !s.hotkey_errors.is_empty() {
                            balloon(s, "Hotkey not available", &s.hotkey_errors.join("\n"), true);
                        }
                    }
                    set_tip(s, &tooltip(s));
                }
            });
            LRESULT(0)
        }
        WM_WORKER => {
            let events: Vec<Event> = EVENTS.lock().unwrap().drain(..).collect();
            with_state(|s| {
                for e in events {
                    match e {
                        Event::Balloon { title, body, error } => balloon(s, &title, &body, error),
                        Event::Saving(t) => toast::show(&t, "", toast::Kind::Info, 0),
                        Event::Saved { title, body } => toast::show(&title, &body, toast::Kind::Ok, 6000),
                        Event::Tip(t) => {
                            let t = if t.is_empty() { tooltip(s) } else { t };
                            set_tip(s, &t);
                        }
                    }
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            with_state(|s| {
                let d = icon_data(s);
                let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m == tb && tb != 0 => {
            // Explorer restarted: put the icon back.
            with_state(|s| add_icon(s));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Run the tray until "Quit". `quiet` = started with Windows (no hello balloon).
pub fn run(quiet: bool) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("LiveVersionsTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Live Versions"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, hinst, None) else {
            return;
        };
        HWND_MAIN.store(super::hwnd_to_isize(hwnd), Ordering::SeqCst);
        toast::init(hinst.into());
        let cx = GetSystemMetrics(SM_CXSMICON);
        let icon = LoadImageW(hinst, PCWSTR(1 as _), IMAGE_ICON, cx, cx, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
        let cfg = config::load();
        let mut st = State {
            hwnd,
            icon,
            cfg,
            cfg_mtime: config::modified(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            browser: None,
            hotkey_errors: vec![],
        };
        register_hotkeys(&mut st);
        add_icon(&st);
        let _ = SetTimer(hwnd, 1, 2000, None);

        if !st.hotkey_errors.is_empty() {
            balloon(&st, "Hotkey not available", &format!("{}. Change it under Settings in the tray menu.", st.hotkey_errors.join("; ")), true);
        } else if !st.cfg.les_warning_shown && st.cfg.hotkey_save_version.eq_ignore_ascii_case("ctrl+alt+s") && super::process_running("live enhancement suite") {
            balloon(
                &st,
                "Live Enhancement Suite is running",
                "LES also saves a version on Ctrl+Alt+S, so both would react. Turn that off in LES, or pick another hotkey in Settings.",
                true,
            );
            st.cfg.les_warning_shown = true;
            config::save(&st.cfg);
            st.cfg_mtime = config::modified();
        } else if !quiet {
            balloon(
                &st,
                "Live Versions is running",
                &format!(
                    "{} in Live saves a new version.\n{} opens the version browser.\nRight-click this icon for more.",
                    pretty_hotkey(&st.cfg.hotkey_save_version),
                    pretty_hotkey(&st.cfg.hotkey_open_browser)
                ),
                false,
            );
        }
        STATE.with(|s| *s.borrow_mut() = Some(st));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        STATE.with(|s| *s.borrow_mut() = None);
    }
}

pub fn spawn_browser_for(path: Option<PathBuf>) {
    let p = path.map(|p| p.to_string_lossy().into_owned());
    let mut args = vec!["--browse"];
    if let Some(p) = &p {
        args.push(p);
    }
    spawn_self(&args);
}
