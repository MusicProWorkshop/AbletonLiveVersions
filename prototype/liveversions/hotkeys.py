"""Global hotkeys via RegisterHotKey on a dedicated message-loop thread (Windows)."""
import ctypes
import threading
from ctypes import wintypes as W

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

MODS = {"alt": 0x1, "ctrl": 0x2, "control": 0x2, "shift": 0x4, "win": 0x8}
MOD_NOREPEAT = 0x4000
WM_HOTKEY = 0x0312
WM_QUIT = 0x0012
NAMED_KEYS = {f"f{i}": 0x6F + i for i in range(1, 25)}
NAMED_KEYS.update({"space": 0x20, "enter": 0x0D, "tab": 0x09, "insert": 0x2D, "home": 0x24,
                   "end": 0x23, "pageup": 0x21, "pagedown": 0x22})


def parse(spec: str):
    parts = [p.strip().lower() for p in spec.split("+") if p.strip()]
    mods, vk = 0, None
    for p in parts:
        if p in MODS:
            mods |= MODS[p]
        elif p in NAMED_KEYS:
            vk = NAMED_KEYS[p]
        elif len(p) == 1 and p.isalnum():
            vk = ord(p.upper())
        else:
            raise ValueError(f"Unknown key '{p}' in hotkey '{spec}'")
    if vk is None:
        raise ValueError(f"Hotkey '{spec}' has no key")
    return mods, vk


class HotkeyThread(threading.Thread):
    """Calls callback(name) for each registered hotkey press. `failed` lists
    hotkeys another program already owns."""

    def __init__(self, bindings: dict, callback):
        super().__init__(daemon=True, name="hotkeys")
        self.bindings = bindings
        self.callback = callback
        self.failed = []
        self.ready = threading.Event()
        self._tid = None

    def run(self):
        self._tid = kernel32.GetCurrentThreadId()
        ids = {}
        for i, (name, spec) in enumerate(self.bindings.items(), start=1):
            try:
                mods, vk = parse(spec)
            except ValueError as e:
                self.failed.append(f"{spec} ({e})")
                continue
            if user32.RegisterHotKey(None, i, mods | MOD_NOREPEAT, vk):
                ids[i] = name
            else:
                self.failed.append(spec)
        self.ready.set()
        msg = W.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            if msg.message == WM_HOTKEY and msg.wParam in ids:
                try:
                    self.callback(ids[msg.wParam])
                except Exception:  # never let a callback kill the hotkey loop
                    pass
        for i in ids:
            user32.UnregisterHotKey(None, i)

    def stop(self):
        if self._tid:
            user32.PostThreadMessageW(self._tid, WM_QUIT, 0, 0)
