"""Background app: global hotkeys + toasts + the version browser, in one process."""
import os
import queue
import threading
import tkinter as tk

from . import config, gui, live, versions
from .hotkeys import HotkeyThread


class Toast:
    """Small always-on-top message in the bottom-right corner."""

    def __init__(self, root):
        self.root = root
        self.win = None
        self._after = None

    def show(self, title, body="", ms=3500, kind="info"):
        if self.win is None or not self.win.winfo_exists():
            self.win = tk.Toplevel(self.root)
            self.win.title("Live Versions notification")
            self.win.overrideredirect(True)
            self.win.attributes("-topmost", True)
            try:
                self.win.attributes("-alpha", 0.96)
            except tk.TclError:
                pass
            self.frame = tk.Frame(self.win, bg=gui.C["panel"], highlightthickness=1,
                                  highlightbackground=gui.C["border"])
            self.frame.pack(fill="both", expand=True)
            self.bar = tk.Frame(self.frame, width=4)
            self.bar.pack(side="left", fill="y")
            inner = tk.Frame(self.frame, bg=gui.C["panel"], padx=14, pady=10)
            inner.pack(side="left", fill="both", expand=True)
            self.t = tk.Label(inner, bg=gui.C["panel"], fg=gui.C["fg"], anchor="w", justify="left",
                              font=(gui.FONT[0], gui.FONT[1] + 1, "bold"))
            self.t.pack(fill="x")
            self.b = tk.Label(inner, bg=gui.C["panel"], fg=gui.C["dim"], anchor="w", justify="left",
                              font=gui.FONT, wraplength=380)
            self.b.pack(fill="x")
            for w in (self.win, self.frame, inner, self.t, self.b):
                w.bind("<Button-1>", lambda e: self.hide())
        color = {"info": gui.C["accent"], "ok": gui.C["good"], "error": "#e06c6c"}[kind]
        self.bar.configure(bg=color)
        self.t.configure(text=title)
        self.b.configure(text=body)
        self.b.pack_forget() if not body else self.b.pack(fill="x")
        self.win.update_idletasks()
        w, h = max(300, self.win.winfo_reqwidth()), self.win.winfo_reqheight()
        sw, sh = self.root.winfo_screenwidth(), self.root.winfo_screenheight()
        self.win.geometry(f"{w}x{h}+{sw - w - 24}+{sh - h - 72}")
        self.win.deiconify()
        self.win.lift()
        if self._after:
            self.root.after_cancel(self._after)
        self._after = self.root.after(ms, self.hide) if ms else None

    def hide(self):
        if self.win is not None and self.win.winfo_exists():
            self.win.withdraw()


class App:
    def __init__(self):
        self.cfg = config.load()
        self.root = tk.Tk()
        self.root.withdraw()
        self.root.title("Live Versions")
        gui.apply_theme(self.root)
        self.toast = Toast(self.root)
        self.q = queue.Queue()
        self.saving = threading.Lock()
        self.browser = None
        self.hk = HotkeyThread({
            "save": self.cfg["hotkey_save_version"],
            "browse": self.cfg["hotkey_open_browser"],
        }, self._on_hotkey)

    # Runs on the hotkey thread: hand everything to the Tk thread.
    # Saving only acts while Live is the active window; the browser opens from anywhere.
    def _on_hotkey(self, name):
        if name == "browse" or live.live_is_foreground():
            self.q.put(("hotkey", name))

    def _poll(self):
        try:
            while True:
                kind, data = self.q.get_nowait()
                if kind == "hotkey" and data == "save":
                    self.start_save()
                elif kind == "hotkey" and data == "browse":
                    self.open_browser()
                elif kind == "toast":
                    self.toast.show(*data)
                elif kind == "saved":
                    if self.browser is not None and self.browser.winfo_exists():
                        self.browser.load(data)
        except queue.Empty:
            pass
        self.root.after(50, self._poll)

    # ---- save a new version
    def start_save(self):
        if not self.saving.acquire(blocking=False):
            return
        _, title = live.live_main_window()
        name, _dirty = live.title_info(title)
        threading.Thread(target=self._save_worker, args=(name,), daemon=True).start()

    def _say(self, title, body="", ms=3500, kind="info"):
        self.q.put(("toast", (title, body, ms, kind)))

    def _save_worker(self, name):
        try:
            if not name or name.lower().startswith("untitled"):
                self._say("Save the set first", "This set has never been saved. Save it once with Ctrl+S, "
                          "then use the version shortcut.", 5000, "error")
                return
            cur = live.current_set_path(name)
            if not cur:
                self._say("Couldn't find this set on disk",
                          f"Live has “{name}” open but its file wasn't found. Save it once with Ctrl+S and retry.",
                          6000, "error")
                return
            new = versions.next_version_path(cur)
            stem = os.path.splitext(os.path.basename(new))[0]
            self._say(f"Saving {stem}…", "", 0)
            live.save_as(new, timeout=float(self.cfg.get("save_timeout_seconds", 120)))
            self._say(f"Saved {stem}", "Writing change notes…", 0, "ok")
            try:
                summary = versions.write_notes(new, parent_path=cur)
            except Exception as e:
                summary = f"(change notes failed: {e})"
            self._say(f"Saved {stem}", summary, 5000, "ok")
            self.q.put(("saved", new))
        except live.LiveError as e:
            self._say("Version not saved", str(e), 6000, "error")
        except Exception as e:
            self._say("Version not saved", f"Unexpected error: {e}", 6000, "error")
        finally:
            self.saving.release()

    # ---- browser
    def open_browser(self):
        path = None
        try:
            path = live.current_set_path()
        except Exception:
            pass
        if self.browser is not None and self.browser.winfo_exists():
            if path:
                self.browser.load(path)
        else:
            self.browser = gui.VersionBrowser(self.root, path, on_close=self._browser_closed)
            if not path:
                self.browser.after(200, self.browser.browse)
        self.browser.deiconify()
        self.browser.lift()
        self.browser.attributes("-topmost", True)
        self.browser.after(300, lambda: self.browser.attributes("-topmost", False))
        self.browser.focus_force()

    def _browser_closed(self):
        self.browser = None

    def run(self):
        self.hk.start()
        self.hk.ready.wait(3)
        save, browse = self.cfg["hotkey_save_version"].upper(), self.cfg["hotkey_open_browser"].upper()
        if self.hk.failed:
            self.toast.show("Live Versions: hotkey unavailable",
                            f"Already used by another program: {', '.join(self.hk.failed)}. "
                            f"Change it in {config.config_path()}", 9000, "error")
        elif not self.cfg.get("les_warning_shown") and save == "CTRL+ALT+S" and les_running():
            # Shown once: we can see that LES runs, not whether its save shortcut is off.
            self.toast.show("Live Enhancement Suite is running",
                            "LES also saves a version on Ctrl+Alt+S, so both would react. Turn that "
                            f"off in LES, or change hotkey_save_version in {config.config_path()}",
                            12000, "error")
            self.cfg["les_warning_shown"] = True
            config.save(self.cfg)
        else:
            self.toast.show("Live Versions is running", f"{save}  save a new version\n{browse}  version browser",
                            4500)
        self.root.after(50, self._poll)
        self.root.mainloop()


def les_running() -> bool:
    """Is Live Enhancement Suite running? (It has its own Ctrl+Alt+S version save.)"""
    import subprocess
    try:
        out = subprocess.run(["tasklist", "/fo", "csv", "/nh"], capture_output=True, text=True,
                             creationflags=0x08000000, timeout=5).stdout
    except Exception:
        return False
    return "live enhancement suite" in out.lower()


def single_instance() -> bool:
    import ctypes
    k = ctypes.WinDLL("kernel32", use_last_error=True)
    App._mutex = k.CreateMutexW(None, False, "Local\\LiveVersionsHotkeys")
    return ctypes.get_last_error() != 183  # ERROR_ALREADY_EXISTS


def main():
    if not live.IS_WIN:
        raise SystemExit("The hotkey service is Windows-only. Use `python -m liveversions gui` instead.")
    if not single_instance():
        import tkinter.messagebox as mb
        r = tk.Tk()
        r.withdraw()
        mb.showinfo("Live Versions", "Live Versions is already running.\n\n"
                    "Press the version-browser hotkey inside Live to open it.")
        r.destroy()
        return
    App().run()
