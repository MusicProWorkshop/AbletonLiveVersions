//! Windows-only pieces: Live integration, tray icon, start-with-Windows, helpers.
pub mod live;
pub mod toast;
pub mod tray;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, IsIconic, SetForegroundWindow, ShowWindow, ASFW_ANY, SW_RESTORE};

/// Make println! work when started from a terminal (the exe has no console of its own).
pub fn attach_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Holds a named mutex for the life of the process. False if another instance owns it.
pub fn single_instance(name: &str) -> bool {
    unsafe {
        let h: windows::core::Result<HANDLE> = CreateMutexW(None, false, &HSTRING::from(name));
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        if let Ok(h) = h {
            if already {
                let _ = CloseHandle(h);
            } else {
                let _ = h; // never closed: the mutex lives as long as the process
            }
        }
        !already
    }
}

pub fn process_running(exe_prefix: &str) -> bool {
    let want = exe_prefix.to_lowercase();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase().starts_with(&want) {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Ask every window of the app (browser, converter, settings) to close: those of
/// other processes running this same exe. Command-line runs have no windows and
/// are left alone. Returns how many windows were asked to close.
pub fn close_app_windows() -> usize {
    let me = std::process::id();
    let my_exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let same_exe = |pid: u32| {
        let e = live::exe_of(pid);
        !e.is_empty()
            && std::fs::canonicalize(&e).map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_else(|_| e.to_lowercase()) == my_exe
    };
    let mut n = 0;
    for (h, _) in live::top_windows() {
        let pid = live::window_pid(h);
        if pid != me && same_exe(pid) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    h,
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
            n += 1;
        }
    }
    n
}

/// Bring a process's main window to the front. False if it has no visible window.
pub fn focus_process(pid: u32) -> bool {
    let Some((h, _)) = live::top_windows().into_iter().find(|(h, t)| live::window_pid(*h) == pid && !t.is_empty()) else {
        return false;
    };
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
    true
}

pub fn allow_foreground() {
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

pub fn hwnd_to_isize(h: HWND) -> isize {
    h.0 as isize
}

pub fn isize_to_hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

// ----------------------------------------------------------------------------- start with Windows

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Live Versions");

fn run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
    let mut k = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, access, &mut k).is_ok().then_some(k) }
}

pub fn startup_enabled() -> bool {
    let Some(k) = run_key(KEY_READ) else { return false };
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe { RegQueryValueExW(k, RUN_VALUE, None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut size)).is_ok() };
    unsafe {
        let _ = RegCloseKey(k);
    }
    if !ok {
        return false;
    }
    // Only "on" if it points at this exe (an old copy elsewhere doesn't count).
    let s = String::from_utf16_lossy(&buf[..(size as usize / 2)]).trim_end_matches('\0').to_lowercase();
    let me = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    s.contains(&me)
}

pub fn set_startup(on: bool) -> bool {
    let Some(k) = run_key(KEY_WRITE) else { return false };
    let ok = unsafe {
        if on {
            let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let cmd: Vec<u16> = format!("\"{exe}\" --startup").encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(cmd.as_ptr() as *const u8, cmd.len() * 2);
            RegSetValueExW(k, RUN_VALUE, 0, REG_SZ, Some(bytes)).is_ok()
        } else {
            let r = RegDeleteValueW(k, RUN_VALUE);
            r.is_ok() || r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
        }
    };
    unsafe {
        let _ = RegCloseKey(k);
    }
    ok
}
