"""Tkinter windows: the version browser and the VST3 conversion dialog."""
import datetime
import os
import queue
import re
import subprocess
import sys
import threading
import tkinter as tk
from tkinter import filedialog, messagebox, ttk

from . import als, changes, config, live, versions, vst3

C = dict(bg="#1e1e1e", panel="#282828", field="#151515", fg="#dddddd", dim="#8f8f8f",
         accent="#ff9b45", accent_hi="#ffb46e", sel="#36506e", border="#3a3a3a",
         good="#8fd18f", warn="#f0c674")
FONT = ("Segoe UI", 10) if sys.platform == "win32" else ("Helvetica", 12)
MONO = ("Consolas", 10) if sys.platform == "win32" else ("Menlo", 11)


def apply_theme(root):
    if sys.platform == "win32":
        try:
            import ctypes
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except Exception:
            pass
    root.configure(bg=C["bg"])
    s = ttk.Style(root)
    s.theme_use("clam")
    s.configure(".", background=C["bg"], foreground=C["fg"], fieldbackground=C["field"],
                bordercolor=C["border"], lightcolor=C["border"], darkcolor=C["border"],
                troughcolor=C["panel"], font=FONT)
    s.configure("Treeview", background=C["field"], fieldbackground=C["field"], foreground=C["fg"],
                rowheight=26, borderwidth=0)
    s.map("Treeview", background=[("selected", C["sel"])], foreground=[("selected", "#ffffff")])
    s.configure("Treeview.Heading", background=C["panel"], foreground=C["dim"], relief="flat",
                padding=(6, 4))
    s.map("Treeview.Heading", background=[("active", C["panel"])])
    s.configure("TButton", background=C["panel"], foreground=C["fg"], padding=(12, 5))
    s.map("TButton", background=[("active", "#383838"), ("disabled", C["bg"])],
          foreground=[("disabled", "#5c5c5c")])
    s.configure("Accent.TButton", background=C["accent"], foreground="#1a1a1a")
    s.map("Accent.TButton", background=[("active", C["accent_hi"]), ("disabled", "#5a4a3a")])
    s.configure("TLabel", background=C["bg"], foreground=C["fg"])
    s.configure("Dim.TLabel", foreground=C["dim"])
    s.configure("Title.TLabel", font=(FONT[0], FONT[1] + 5, "bold"))
    s.configure("Panel.TFrame", background=C["panel"])
    s.configure("TCombobox", fieldbackground=C["field"], background=C["panel"], foreground=C["fg"],
                arrowcolor=C["fg"])
    s.map("TCombobox", fieldbackground=[("readonly", C["field"])], foreground=[("readonly", C["fg"])])
    s.configure("TPanedwindow", background=C["bg"])
    s.configure("Vertical.TScrollbar", background=C["panel"], arrowcolor=C["dim"])
    root.option_add("*TCombobox*Listbox.background", C["field"])
    root.option_add("*TCombobox*Listbox.foreground", C["fg"])
    root.option_add("*TCombobox*Listbox.selectBackground", C["sel"])


def _text_widget(parent, **kw):
    t = tk.Text(parent, bg=C["field"], fg=C["fg"], insertbackground=C["fg"], relief="flat",
                highlightthickness=1, highlightbackground=C["border"], highlightcolor=C["accent"],
                padx=10, pady=8, wrap="word", font=MONO, **kw)
    t.tag_configure("h", foreground=C["accent"])
    t.tag_configure("dim", foreground=C["dim"])
    t.tag_configure("add", foreground=C["good"])
    t.tag_configure("del", foreground="#e88b8b")
    return t


def _show_text(widget, text):
    widget.configure(state="normal")
    widget.delete("1.0", "end")
    for line in text.splitlines():
        s = line.strip()
        if s and set(s) == {"="}:
            continue  # title underline is for the .txt file only
        tag = ()
        if s and not line.startswith(" ") and not s.startswith(("Saved", "Compared", "Summary", "Ableton")):
            tag = ("h",)
        elif s.startswith(("Saved", "Compared", "Ableton", "=")):
            tag = ("dim",)
        elif s.startswith("+ ") or s.startswith("✓"):
            tag = ("add",)
        elif s.startswith("− "):
            tag = ("del",)
        widget.insert("end", line + "\n", tag)
    widget.configure(state="disabled")


def open_path(path):
    if sys.platform == "win32":
        os.startfile(path)
    else:
        subprocess.Popen(["open", path])


def reveal(path):
    if sys.platform == "win32":
        subprocess.Popen(["explorer", "/select,", os.path.normpath(path)])
    else:
        subprocess.Popen(["open", "-R", path])


# ----------------------------------------------------------------------------- browser

class VersionBrowser(tk.Toplevel):
    def __init__(self, master, path=None, on_close=None):
        super().__init__(master)
        self.cfg = config.load()
        self.on_close = on_close
        self.title("Live Versions")
        self.configure(bg=C["bg"])
        self.geometry("1180x680")
        self.minsize(820, 460)
        self.q = queue.Queue()
        self.folder = None
        self.family = []
        self.base = None
        self.current_open = None
        self._job = 0
        self._build()
        self.protocol("WM_DELETE_WINDOW", self.close)
        self.after(60, self._poll)
        if path:
            self.load(path)

    # ---- layout
    def _build(self):
        top = ttk.Frame(self, padding=(16, 14, 16, 8))
        top.pack(fill="x")
        self.title_lbl = ttk.Label(top, text="No set loaded", style="Title.TLabel")
        self.title_lbl.grid(row=0, column=0, sticky="w")
        self.family_box = ttk.Combobox(top, state="readonly", width=34)
        self.family_box.grid(row=0, column=1, padx=(16, 0), sticky="w")
        self.family_box.bind("<<ComboboxSelected>>", self._family_changed)
        ttk.Button(top, text="Open set or folder…", command=self.browse).grid(row=0, column=3, padx=(8, 0))
        ttk.Button(top, text="Refresh", command=self.refresh).grid(row=0, column=4, padx=(8, 0))
        top.columnconfigure(2, weight=1)
        self.folder_lbl = ttk.Label(top, text="", style="Dim.TLabel")
        self.folder_lbl.grid(row=1, column=0, columnspan=5, sticky="w", pady=(4, 0))

        pw = ttk.Panedwindow(self, orient="horizontal")
        pw.pack(fill="both", expand=True, padx=16, pady=(4, 8))
        left = ttk.Frame(pw)
        cols = ("ver", "saved", "tracks", "summary")
        self.tree = ttk.Treeview(left, columns=cols, show="headings", selectmode="extended")
        for c, w, t in (("ver", 90, "Version"), ("saved", 120, "Saved"), ("tracks", 60, "Tracks"),
                        ("summary", 420, "What changed")):
            self.tree.heading(c, text=t, anchor="w")
            self.tree.column(c, width=w, anchor="w", stretch=(c == "summary"))
        self.tree.tag_configure("open", foreground=C["accent"])
        self.tree.tag_configure("pending", foreground=C["dim"])
        sb = ttk.Scrollbar(left, orient="vertical", command=self.tree.yview)
        self.tree.configure(yscrollcommand=sb.set)
        self.tree.pack(side="left", fill="both", expand=True)
        sb.pack(side="right", fill="y")
        self.tree.bind("<<TreeviewSelect>>", self._selected)
        self.tree.bind("<Double-1>", lambda e: self.open_selected())
        self.tree.bind("<Return>", lambda e: self.open_selected())
        pw.add(left, weight=3)

        right = ttk.Frame(pw)
        self.details = _text_widget(right, height=10, state="disabled")
        self.details.pack(fill="both", expand=True)
        nl = ttk.Frame(right)
        nl.pack(fill="x", pady=(10, 4))
        ttk.Label(nl, text="Your notes for this version", style="Dim.TLabel").pack(side="left")
        self.save_note_btn = ttk.Button(nl, text="Save note", command=self.save_note, state="disabled")
        self.save_note_btn.pack(side="right")
        self.note = _text_widget(right, height=5)
        self.note.configure(font=FONT)
        self.note.pack(fill="x")
        self.note.bind("<KeyRelease>", lambda e: self.save_note_btn.configure(
            state="normal" if len(self.tree.selection()) == 1 else "disabled"))
        pw.add(right, weight=2)

        bar = ttk.Frame(self, padding=(16, 0, 16, 14))
        bar.pack(fill="x")
        self.open_btn = ttk.Button(bar, text="Open in Live", style="Accent.TButton", command=self.open_selected)
        self.open_btn.pack(side="left")
        self.cmp_btn = ttk.Button(bar, text="Compare selected", command=self.compare)
        self.cmp_btn.pack(side="left", padx=(8, 0))
        self.regen_btn = ttk.Button(bar, text="Regenerate notes", command=self.regenerate)
        self.regen_btn.pack(side="left", padx=(8, 0))
        self.reveal_btn = ttk.Button(bar, text="Show in folder", command=self.reveal_selected)
        self.reveal_btn.pack(side="left", padx=(8, 0))
        self.vst3_btn = ttk.Button(bar, text="Convert to VST3…", command=self.convert_selected)
        self.vst3_btn.pack(side="left", padx=(8, 0))
        self.status = ttk.Label(bar, text="", style="Dim.TLabel")
        self.status.pack(side="right")
        self._update_buttons()

    # ---- loading
    def browse(self):
        p = filedialog.askopenfilename(parent=self, title="Open an Ableton Live set",
                                       filetypes=[("Live Set", "*.als")])
        if p:
            self.load(p)

    def load(self, path):
        path = os.path.abspath(path)
        if os.path.isdir(path):
            fams = versions.families_in(path)
            if not fams:
                messagebox.showinfo("Live Versions", "No Live sets in that folder.", parent=self)
                return
            base = max(fams, key=lambda b: max(os.path.getmtime(v.path) for v in fams[b]))
            path = fams[base][-1].path
        self.folder = os.path.dirname(path)
        self.base = versions.parse(path).base.strip()
        self._select_after_load = path
        fams = versions.families_in(self.folder)
        self.family_box["values"] = [f"{b}  ({len(v)})" for b, v in fams.items()]
        self._fam_keys = list(fams)
        if self.base in self._fam_keys:
            self.family_box.current(self._fam_keys.index(self.base))
        self.refresh()

    def _family_changed(self, _):
        i = self.family_box.current()
        if i >= 0:
            fam = versions.families_in(self.folder)[self._fam_keys[i]]
            self.load(fam[-1].path)

    def refresh(self):
        if not self.folder:
            return
        keep = set(self.tree.selection()) or {getattr(self, "_select_after_load", None)}
        self._select_after_load = None
        self.family = [v for v in versions.families_in(self.folder).get(self.base, [])]
        self.title_lbl.configure(text=self.base)
        self.title(f"Live Versions — {self.base}")
        f = self.folder
        self.folder_lbl.configure(text=f if len(f) <= 120 else f[:40] + " … " + f[-76:])
        try:
            self.current_open = live.current_set_path() if live.IS_WIN else None
        except Exception:
            self.current_open = None
        self.tree.delete(*self.tree.get_children())
        todo = []
        for v in reversed(self.family):
            summary = versions.summary_from_notes(v.path)
            text = versions.read_notes(v.path)["text"]
            m = re.search(r"· (\d+) tracks", text)
            saved = datetime.datetime.fromtimestamp(os.path.getmtime(v.path)).strftime("%d %b %Y %H:%M")
            is_open = self.current_open and os.path.normcase(self.current_open) == os.path.normcase(v.path)
            label = ("▶ " if is_open else "") + v.label
            tags = ("open",) if is_open else ()
            if summary is None:
                cloud = als.is_cloud_only(v.path)
                summary = "in the cloud only — select and click Regenerate notes" if cloud else "reading…"
                tags = tags + ("pending",)
                if not cloud and self.cfg.get("auto_write_missing_notes", True):
                    todo.append(v.path)
            self.tree.insert("", "end", iid=v.path, values=(label, saved, m.group(1) if m else "", summary),
                             tags=tags)
        sel = [p for p in keep if p and self.tree.exists(p)]
        if sel:
            self.tree.selection_set(sel)
            self.tree.see(sel[0])
        elif self.tree.get_children():
            first = self.tree.get_children()[0]
            self.tree.selection_set(first)
        self.status.configure(text=f"{len(self.family)} versions")
        if todo:
            self._run_notes(list(reversed(todo)))  # oldest first: parents get cached first

    def _run_notes(self, paths):
        self._job += 1
        job = self._job

        def work():
            for i, p in enumerate(paths, 1):
                if job != self._job:
                    return
                self.q.put(("status", f"Writing notes {i}/{len(paths)}…"))
                try:
                    versions.write_notes(p)
                    self.q.put(("row", p))
                except Exception as e:
                    self.q.put(("error", f"{os.path.basename(p)}: {e}"))
            self.q.put(("status", f"{len(self.family)} versions"))

        threading.Thread(target=work, daemon=True).start()

    def _poll(self):
        try:
            while True:
                kind, data = self.q.get_nowait()
                if kind == "status":
                    self.status.configure(text=data)
                elif kind == "row" and self.tree.exists(data):
                    text = versions.read_notes(data)["text"]
                    m = re.search(r"· (\d+) tracks", text)
                    vals = list(self.tree.item(data, "values"))
                    vals[2] = m.group(1) if m else ""
                    vals[3] = versions.summary_from_notes(data) or ""
                    tags = tuple(t for t in self.tree.item(data, "tags") if t != "pending")
                    self.tree.item(data, values=vals, tags=tags)
                    if self.tree.selection() == (data,):
                        self._selected()
                elif kind == "error":
                    self.status.configure(text=data)
                elif kind == "callback":
                    data()
        except queue.Empty:
            pass
        if self.winfo_exists():
            self.after(80, self._poll)

    # ---- selection
    def _sel(self):
        return list(self.tree.selection())

    def _update_buttons(self):
        n = len(self.tree.selection()) if hasattr(self, "tree") else 0
        for b in (self.open_btn, self.reveal_btn, self.vst3_btn, self.regen_btn):
            b.configure(state="normal" if n == 1 else "disabled")
        self.cmp_btn.configure(state="normal" if n == 2 else "disabled")

    def _selected(self, _=None):
        self._update_buttons()
        sel = self._sel()
        self.note.delete("1.0", "end")
        self.save_note_btn.configure(state="disabled")
        if len(sel) == 1:
            n = versions.read_notes(sel[0])
            _show_text(self.details, n["text"] or "No notes for this version yet.\n\nClick “Regenerate notes”.")
            self.note.insert("1.0", n["user"])
        elif len(sel) == 2:
            _show_text(self.details, "Two versions selected.\n\nClick “Compare selected” to see what changed between them.")
        else:
            _show_text(self.details, "")

    # ---- actions
    def open_selected(self):
        sel = self._sel()
        if len(sel) == 1:
            open_path(sel[0])
            self.status.configure(text=f"Opening {os.path.basename(sel[0])} in Live…")

    def reveal_selected(self):
        sel = self._sel()
        if len(sel) == 1:
            reveal(sel[0])

    def save_note(self):
        sel = self._sel()
        if len(sel) == 1:
            versions.save_user_note(sel[0], self.note.get("1.0", "end").strip())
            self.save_note_btn.configure(state="disabled")
            self.status.configure(text="Note saved")

    def regenerate(self):
        sel = self._sel()
        if len(sel) == 1:
            self.tree.set(sel[0], "summary", "reading…")
            self._run_notes(sel)

    def compare(self):
        sel = self._sel()
        if len(sel) != 2:
            return
        a, b = sorted(sel, key=lambda p: versions.parse(p).num)
        _show_text(self.details, "Comparing…")

        def work():
            try:
                ch = changes.diff(als.snapshot(a), als.snapshot(b))
                text = "\n".join([f"{os.path.splitext(os.path.basename(a))[0]}  →  "
                                  f"{os.path.splitext(os.path.basename(b))[0]}", "",
                                  f"Summary: {changes.summary_line(ch)}", ""] + changes.format_lines(ch))
            except Exception as e:
                text = f"Couldn't compare: {e}"
            self.q.put(("callback", lambda: _show_text(self.details, text)))

        threading.Thread(target=work, daemon=True).start()

    def convert_selected(self):
        sel = self._sel()
        if len(sel) == 1:
            ConvertDialog(self, sel[0], on_done=lambda out: (self.refresh(), self._select(out)))

    def _select(self, path):
        if self.tree.exists(path):
            self.tree.selection_set(path)
            self.tree.see(path)

    def close(self):
        self._job += 1
        self.destroy()
        if self.on_close:
            self.on_close()


# ----------------------------------------------------------------------------- VST3 dialog

class ConvertDialog(tk.Toplevel):
    def __init__(self, master, path, on_done=None):
        super().__init__(master)
        self.path = path
        self.on_done = on_done
        self.plan = None
        self.q = queue.Queue()
        self.title("Convert plug-ins to VST3")
        self.configure(bg=C["bg"])
        self.geometry("1100x560")
        self.transient(master)
        frm = ttk.Frame(self, padding=16)
        frm.pack(fill="both", expand=True)
        ttk.Label(frm, text=os.path.splitext(os.path.basename(path))[0], style="Title.TLabel").pack(anchor="w")
        self.info = ttk.Label(frm, text="Reading the set and your installed VST3 plug-ins…", style="Dim.TLabel",
                              wraplength=1000, justify="left")
        self.info.pack(anchor="w", pady=(2, 10))
        cols = ("track", "old", "new", "how", "auto")
        self.tree = ttk.Treeview(frm, columns=cols, show="headings", selectmode="browse", height=12)
        for c, w, t in (("track", 170, "Track"), ("old", 210, "Current plug-in"), ("new", 200, "VST3"),
                        ("how", 330, "Settings"), ("auto", 150, "Automation / mappings")):
            self.tree.heading(c, text=t, anchor="w")
            self.tree.column(c, width=w, anchor="w")
        self.tree.tag_configure("skip", foreground=C["dim"])
        self.tree.tag_configure("good", foreground=C["good"])
        self.tree.tag_configure("warn", foreground=C["warn"])
        self.tree.pack(fill="both", expand=True)
        self.tree.bind("<<TreeviewSelect>>", self._row_selected)

        row = ttk.Frame(frm)
        row.pack(fill="x", pady=(10, 0))
        ttk.Label(row, text="For the selected plug-in:").pack(side="left")
        self.mode_box = ttk.Combobox(row, state="readonly", width=52)
        self.mode_box.pack(side="left", padx=8)
        self.mode_box.bind("<<ComboboxSelected>>", self._mode_changed)
        self.note_lbl = ttk.Label(row, text="", style="Dim.TLabel")
        self.note_lbl.pack(side="left", padx=8)

        bar = ttk.Frame(frm)
        bar.pack(fill="x", pady=(14, 0))
        self.go = ttk.Button(bar, text="Convert → save as new version", style="Accent.TButton",
                             command=self.convert, state="disabled")
        self.go.pack(side="left")
        ttk.Button(bar, text="Learn parameter IDs from folder…", command=self.learn).pack(side="left", padx=8)
        ttk.Button(bar, text="Close", command=self.destroy).pack(side="right")
        self.status = ttk.Label(bar, text="", style="Dim.TLabel")
        self.status.pack(side="right", padx=12)
        self.after(60, self._poll)
        self._analyze()

    def _analyze(self):
        def work():
            try:
                plan = vst3.analyze(self.path)
                self.q.put(("plan", plan))
            except Exception as e:
                self.q.put(("error", str(e)))
        threading.Thread(target=work, daemon=True).start()

    def _poll(self):
        try:
            while True:
                kind, data = self.q.get_nowait()
                if kind == "plan":
                    self.plan = data
                    self._fill()
                elif kind == "error":
                    self.info.configure(text=f"Error: {data}")
                elif kind == "status":
                    self.status.configure(text=data)
                elif kind == "done":
                    out, report = data
                    self.status.configure(text="")
                    messagebox.showinfo("Converted", f"Saved as {os.path.basename(out)}\n\n" + "\n".join(report[:30]),
                                        parent=self)
                    if self.on_done:
                        self.on_done(out)
                    self.destroy()
                    return
                elif kind == "learned":
                    self.status.configure(text=f"Learned {data} parameter IDs")
                    self._analyze()
        except queue.Empty:
            pass
        if self.winfo_exists():
            self.after(80, self._poll)

    def _auto_text(self, it):
        if not it.automated:
            return "—"
        if it.mode == "skip":
            return f"{len(it.automated)} (unchanged)"
        k = len(it.mapped)
        return f"{k}/{len(it.automated)} kept" + ("" if k == len(it.automated) else " ⚠")

    def _fill(self):
        self.tree.delete(*self.tree.get_children())
        if not self.plan.items:
            self.info.configure(text="This set has no VST2 or Audio Unit plug-ins. Nothing to convert.")
            return
        conv = sum(1 for i in self.plan.items if i.mode != "skip")
        self.info.configure(text=(
            f"{len(self.plan.items)} VST2/AU plug-ins found, {conv} can be replaced "
            f"({self.plan.catalog_size} VST3s installed). The original set is not changed: the result is saved "
            f"as the next version. Automation needs the VST3's parameter IDs; they are learned from any set "
            f"that already uses that VST3."))
        for i, it in enumerate(self.plan.items):
            tag = "skip" if it.mode == "skip" else ("good" if it.mode == "vst2state" else "warn")
            self.tree.insert("", "end", iid=str(i), tags=(tag,),
                             values=(it.track, it.old_label, it.new_label, MODES_SHORT[it.mode], self._auto_text(it)))
        self.go.configure(state="normal" if conv else "disabled")
        self.tree.selection_set("0")

    def _row_selected(self, _=None):
        sel = self.tree.selection()
        if not sel or not self.plan:
            return
        it = self.plan.items[int(sel[0])]
        modes = it.modes()
        self._modes = modes
        self.mode_box["values"] = [vst3.MODES[m] for m in modes]
        self.mode_box.current(modes.index(it.mode) if it.mode in modes else len(modes) - 1)
        self.note_lbl.configure(text=it.note)

    def _mode_changed(self, _=None):
        sel = self.tree.selection()
        if not sel:
            return
        idx = int(sel[0])
        it = self.plan.items[idx]
        it.mode = self._modes[self.mode_box.current()]
        tag = "skip" if it.mode == "skip" else ("good" if it.mode == "vst2state" else "warn")
        self.tree.item(sel[0], tags=(tag,), values=(it.track, it.old_label, it.new_label,
                                                    MODES_SHORT[it.mode], self._auto_text(it)))
        conv = sum(1 for i in self.plan.items if i.mode != "skip")
        self.go.configure(state="normal" if conv else "disabled")

    def convert(self):
        if not self.plan:
            return
        self.go.configure(state="disabled")
        self.status.configure(text="Converting…")
        plan = self.plan

        def work():
            try:
                out, report = vst3.convert(plan)
                versions.write_notes(out, parent_path=plan.path, extra_lines=report)
                self.q.put(("done", (out, report)))
            except Exception as e:
                self.q.put(("error", f"Conversion failed: {e}"))
        threading.Thread(target=work, daemon=True).start()

    def learn(self):
        folder = filedialog.askdirectory(parent=self, title="Folder with sets that use the VST3 versions")
        if not folder:
            return

        def work():
            n = vst3.learn_paths([folder], progress=lambda i, t, f: self.q.put(("status", f"Learning {i}/{t}…")))
            self.q.put(("learned", n))
        threading.Thread(target=work, daemon=True).start()


MODES_SHORT = {
    "vst2state": "Settings kept (VST2-compatible VST3)",
    "raw": "Old data passed as-is — check in Live",
    "sidebyside": "Defaults + old plug-in kept (off)",
    "default": "Default settings",
    "skip": "Not converted",
}


def run_standalone(path=None):
    root = tk.Tk()
    root.withdraw()
    apply_theme(root)
    if path is None and live.IS_WIN:
        try:
            path = live.current_set_path()
        except Exception:
            path = None
    b = VersionBrowser(root, path, on_close=root.destroy)
    if not path:
        b.after(200, b.browse)
    root.mainloop()
