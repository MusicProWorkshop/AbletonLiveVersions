"""Talking to a running Ableton Live on Windows (ctypes only, no extra packages).

* which set is open: Live keeps the open document's full path in
  %APPDATA%/Ableton/Live x.y.z/Preferences/CrashRecoveryInfo.cfg (UTF-16), and the
  window title is "<set name>[*] - Ableton Live 12 Suite";
* saving a new version: press Ctrl+Shift+S in Live, put the full new path into
  the file-name box of the Save dialog and confirm.
"""
import glob
import os
import re
import sys
import time

IS_WIN = sys.platform == "win32"

if IS_WIN:
    import ctypes
    from ctypes import wintypes as W

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    ULONG_PTR = ctypes.c_size_t

    class KEYBDINPUT(ctypes.Structure):
        _fields_ = [("wVk", W.WORD), ("wScan", W.WORD), ("dwFlags", W.DWORD),
                    ("time", W.DWORD), ("dwExtraInfo", ULONG_PTR)]

    class MOUSEINPUT(ctypes.Structure):
        _fields_ = [("dx", W.LONG), ("dy", W.LONG), ("mouseData", W.DWORD),
                    ("dwFlags", W.DWORD), ("time", W.DWORD), ("dwExtraInfo", ULONG_PTR)]

    class _INPUTUNION(ctypes.Union):
        _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT)]

    class INPUT(ctypes.Structure):
        _fields_ = [("type", W.DWORD), ("u", _INPUTUNION)]

    user32.SendInput.argtypes = (W.UINT, ctypes.POINTER(INPUT), ctypes.c_int)
    user32.SendMessageW.argtypes = (W.HWND, W.UINT, W.WPARAM, W.LPARAM)
    user32.SendMessageW.restype = ctypes.c_ssize_t
    user32.PostMessageW.argtypes = (W.HWND, W.UINT, W.WPARAM, W.LPARAM)
    user32.GetDlgCtrlID.argtypes = (W.HWND,)
    user32.GetParent.argtypes = (W.HWND,)
    user32.GetParent.restype = W.HWND
    user32.GetForegroundWindow.restype = W.HWND
    WNDENUMPROC = ctypes.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)

KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004
VK = {"ctrl": 0x11, "shift": 0x10, "alt": 0x12, "win": 0x5B, "enter": 0x0D, "a": 0x41,
      "s": 0x53, "esc": 0x1B}
WM_SETTEXT = 0x000C
BM_CLICK = 0x00F5
LIVE_TITLE_RE = re.compile(r"^(?P<name>.*?)(?P<dirty>\*)?(?: \[[^\]]*\])? - Ableton Live\b")


class LiveError(Exception):
    """Something the user should be told about (shown as a toast)."""


# ----------------------------------------------------------------------------- windows

def _text(hwnd) -> str:
    n = user32.GetWindowTextLengthW(hwnd)
    buf = ctypes.create_unicode_buffer(n + 1)
    user32.GetWindowTextW(hwnd, buf, n + 1)
    return buf.value


def _class(hwnd) -> str:
    buf = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(hwnd, buf, 256)
    return buf.value


def _pid(hwnd) -> int:
    pid = W.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def _exe(pid: int) -> str:
    h = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return ""
    try:
        buf = ctypes.create_unicode_buffer(1024)
        size = W.DWORD(1024)
        if kernel32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
            return buf.value
        return ""
    finally:
        kernel32.CloseHandle(h)


def _is_live_exe(path: str) -> bool:
    return os.path.basename(path).lower().startswith("ableton live")


def live_main_window():
    """(hwnd, title) of Live's main window, or (None, None)."""
    found = []

    def cb(hwnd, _):
        if user32.IsWindowVisible(hwnd):
            t = _text(hwnd)
            if LIVE_TITLE_RE.match(t) and _is_live_exe(_exe(_pid(hwnd))):
                found.append((hwnd, t))
        return True

    user32.EnumWindows(WNDENUMPROC(cb), 0)
    return found[0] if found else (None, None)


def live_is_foreground() -> bool:
    if not IS_WIN:
        return False
    hwnd = user32.GetForegroundWindow()
    return bool(hwnd) and _is_live_exe(_exe(_pid(hwnd)))


def title_info(title: str):
    m = LIVE_TITLE_RE.match(title or "")
    if not m:
        return None, False
    return m.group("name").strip(), bool(m.group("dirty"))


# ----------------------------------------------------------------------------- which set is open

def _pref_dirs():
    base = os.path.join(os.environ.get("APPDATA", ""), "Ableton")
    dirs = [d for d in glob.glob(os.path.join(base, "Live *", "Preferences")) if os.path.isdir(d)]
    return sorted(dirs, key=lambda d: os.path.getmtime(d), reverse=True)


def _paths_in_crash_info(pref_dir: str):
    try:
        with open(os.path.join(pref_dir, "CrashRecoveryInfo.cfg"), "rb") as f:
            data = f.read()
    except OSError:
        return []
    out = []
    for off in (0, 1):
        text = data[off:].decode("utf-16-le", "ignore")
        out += re.findall(r"[A-Za-z]:[/\\][^\x00-\x1f]*?\.als", text)
    pref_norm = os.path.normcase(os.path.abspath(pref_dir))
    return [p for p in out if not os.path.normcase(os.path.abspath(p)).startswith(pref_norm)]


def _paths_in_log(pref_dir: str):
    try:
        with open(os.path.join(pref_dir, "Log.txt"), encoding="utf-8", errors="ignore") as f:
            text = f.read()
    except OSError:
        return []
    return list(reversed(re.findall(r'Loading document "([^"]+\.als)"', text)))


def current_set_path(title_name=None):
    """Full path of the set open in Live, checked against the window title."""
    if title_name is None and IS_WIN:
        _, title = live_main_window()
        title_name, _ = title_info(title)
    candidates = []
    for d in _pref_dirs():
        candidates += _paths_in_crash_info(d)
    for d in _pref_dirs()[:1]:
        candidates += _paths_in_log(d)
    for p in candidates:
        p = os.path.normpath(p)
        stem = os.path.splitext(os.path.basename(p))[0]
        if (title_name is None or stem == title_name) and os.path.exists(p):
            return p
    return None


# ----------------------------------------------------------------------------- keyboard

def _send(inputs):
    arr = (INPUT * len(inputs))(*inputs)
    user32.SendInput(len(inputs), arr, ctypes.sizeof(INPUT))


def _key(vk, up=False):
    return INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(vk, 0, KEYEVENTF_KEYUP if up else 0, 0, 0)))


def press(*names):
    vks = [VK[n] for n in names]
    _send([_key(v) for v in vks] + [_key(v, True) for v in reversed(vks)])


def type_text(text: str):
    seq = []
    for ch in text:
        code = ord(ch)
        seq.append(INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(0, code, KEYEVENTF_UNICODE, 0, 0))))
        seq.append(INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(0, code, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, 0, 0))))
    _send(seq)


def wait_modifiers_released(timeout=1.5):
    """The user is still holding Ctrl+Alt from the hotkey; Live would see
    Ctrl+Alt+Shift+S. Wait for release, then force key-ups as a fallback."""
    end = time.time() + timeout
    mods = [VK["ctrl"], VK["alt"], VK["shift"], VK["win"], 0x5C]
    while time.time() < end:
        if not any(user32.GetAsyncKeyState(v) & 0x8000 for v in mods):
            return
        time.sleep(0.02)
    _send([_key(v, True) for v in mods])


# ----------------------------------------------------------------------------- Save As dialog

def _children(hwnd):
    out = []

    def cb(h, _):
        out.append(h)
        return True

    user32.EnumChildWindows(hwnd, WNDENUMPROC(cb), 0)
    return out


def _filename_edit(dialog):
    """The 'File name' box of a common Save dialog: Edit inside ComboBox inside ComboBoxEx32."""
    for h in _children(dialog):
        if _class(h) != "Edit":
            continue
        p = user32.GetParent(h)
        if p and _class(p) == "ComboBox":
            gp = user32.GetParent(p)
            if gp and _class(gp) == "ComboBoxEx32":
                return h
    return None


def _ok_button(dialog):
    for h in _children(dialog):
        if _class(h) == "Button" and user32.GetDlgCtrlID(h) == 1 and user32.IsWindowVisible(h):
            return h
    return None


def wait_for_dialog(owner_pid: int, main_hwnd, timeout=6.0):
    """A new foreground window of Live's process: normally the common '#32770'
    Save dialog; anything else is handled by typing into the focused field."""
    end = time.time() + timeout
    while time.time() < end:
        h = user32.GetForegroundWindow()
        if h and h != main_hwnd and _pid(h) == owner_pid:
            if _class(h) == "#32770":
                return h
            time.sleep(0.4)  # give a non-standard dialog a moment to take focus
            if user32.GetForegroundWindow() == h:
                return h
        time.sleep(0.05)
    return None


def fill_save_dialog(dialog, full_path: str) -> None:
    edit = _filename_edit(dialog)
    if edit:
        buf = ctypes.create_unicode_buffer(full_path)
        user32.SendMessageW(edit, WM_SETTEXT, 0, ctypes.cast(buf, ctypes.c_void_p).value)
        time.sleep(0.05)
        ok = _ok_button(dialog)
        if ok:
            user32.PostMessageW(ok, BM_CLICK, 0, 0)
            return
        user32.SetForegroundWindow(dialog)
        press("enter")
        return
    # Unknown dialog layout: the file-name box has focus with its text selected.
    press("ctrl", "a")
    type_text(full_path)
    time.sleep(0.05)
    press("enter")


def wait_for_file(path: str, since: float, timeout: float) -> bool:
    """Until the file exists, is newer than `since`, and its size stopped changing."""
    end = time.time() + timeout
    last = -1
    stable = 0
    while time.time() < end:
        try:
            st = os.stat(path)
            if st.st_mtime >= since - 2:
                if st.st_size == last and st.st_size > 0:
                    stable += 1
                    if stable >= 3:
                        with open(path, "rb"):
                            return True
                else:
                    stable = 0
                last = st.st_size
        except OSError:
            pass
        time.sleep(0.25)
    return False


def save_as(new_path: str, timeout: float = 120.0, window=None) -> None:
    """Drive Live's Save As to write `new_path`. Live then has the new file open.
    `window` targets another app's main window (used for testing)."""
    main = window or live_main_window()[0]
    if not main:
        raise LiveError("Ableton Live isn't running.")
    pid = _pid(main)
    wait_modifiers_released()
    if user32.GetForegroundWindow() != main:
        user32.SetForegroundWindow(main)
        time.sleep(0.15)
    if user32.GetForegroundWindow() != main:
        # Never send Ctrl+Shift+S to whatever else happens to be focused.
        raise LiveError("Live isn't the active window — click into Live and try again.")
    started = time.time()
    press("ctrl", "shift", "s")
    dialog = wait_for_dialog(pid, main)
    if not dialog:
        raise LiveError("Live's Save dialog didn't open (is another Live dialog open?).")
    time.sleep(0.15)  # let the dialog finish populating
    fill_save_dialog(dialog, new_path)
    if not wait_for_file(new_path, started, timeout):
        raise LiveError(f"Live didn't write {os.path.basename(new_path)} — check Live for a message.")
