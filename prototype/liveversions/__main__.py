"""Command line.

  python -m liveversions                 start the hotkey service (Windows)
  python -m liveversions gui [set|dir]   version browser
  python -m liveversions notes SET...    (re)write change notes
  python -m liveversions diff A B        print what changed between two sets
  python -m liveversions vst3 SET [--dry-run] [--out PATH] [--mode NAME=MODE ...]
  python -m liveversions learn FOLDER... learn VST3 parameter IDs from sets
  python -m liveversions startup install|remove
"""
import argparse
import os
import sys


def _utf8():
    for s in (sys.stdout, sys.stderr):
        try:
            s.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass


def main(argv=None):
    _utf8()
    ap = argparse.ArgumentParser(prog="liveversions", description="Version manager for Ableton Live sets")
    sub = ap.add_subparsers(dest="cmd")
    sub.add_parser("run", help="start the hotkey service (default)")
    g = sub.add_parser("gui", help="open the version browser")
    g.add_argument("path", nargs="?")
    n = sub.add_parser("notes", help="write change notes for sets")
    n.add_argument("sets", nargs="+")
    d = sub.add_parser("diff", help="show changes between two sets")
    d.add_argument("old")
    d.add_argument("new")
    v = sub.add_parser("vst3", help="replace VST2/AU plug-ins with VST3")
    v.add_argument("set")
    v.add_argument("--dry-run", action="store_true", help="only show what would happen")
    v.add_argument("--out", help="output path (default: next version next to the set)")
    v.add_argument("--mode", action="append", default=[], metavar="PLUGIN=MODE",
                   help="override per plug-in name, modes: vst2state raw sidebyside default skip")
    l = sub.add_parser("learn", help="learn VST3 parameter IDs from sets in folders")
    l.add_argument("folders", nargs="+")
    s = sub.add_parser("startup", help="start the hotkey service when you log in (Windows)")
    s.add_argument("action", choices=["install", "remove"])
    a = ap.parse_args(argv)

    if a.cmd in (None, "run"):
        from . import app
        app.main()
    elif a.cmd == "gui":
        from . import gui
        gui.run_standalone(a.path)
    elif a.cmd == "notes":
        from . import versions
        for p in a.sets:
            print(f"{os.path.basename(p)}: {versions.write_notes(p)}")
            print(f"  -> {versions.notes_path(p)}")
    elif a.cmd == "diff":
        from . import als, changes
        ch = changes.diff(als.snapshot(a.old), als.snapshot(a.new))
        print("Summary:", changes.summary_line(ch), "\n")
        print("\n".join(changes.format_lines(ch)))
    elif a.cmd == "vst3":
        cmd_vst3(a)
    elif a.cmd == "learn":
        from . import vst3
        n = vst3.learn_paths(a.folders, progress=lambda i, t, f: print(f"\r{i + 1}/{t}", end="", flush=True))
        print(f"\nLearned {n} new parameter IDs.")
    elif a.cmd == "startup":
        cmd_startup(a.action)


def cmd_vst3(a):
    from . import versions, vst3
    plan = vst3.analyze(a.set)
    overrides = {}
    for m in a.mode:
        name, _, mode = m.rpartition("=")
        if mode not in vst3.MODES:
            raise SystemExit(f"Unknown mode '{mode}'. Use one of: {', '.join(vst3.MODES)}")
        overrides[name.lower()] = mode
    print(f"{len(plan.items)} VST2/AU plug-ins, {plan.catalog_size} VST3s installed\n")
    for it in plan.items:
        if it.src.name.lower() in overrides and overrides[it.src.name.lower()] in it.modes():
            it.mode = overrides[it.src.name.lower()]
        auto = f"  automation {len(it.mapped)}/{len(it.automated)}" if it.automated else ""
        print(f"  {it.track[:24]:24} {it.old_label:34} → {it.new_label:28} {it.mode:10}{auto}")
        print(f"  {'':24} {it.note}")
    if a.dry_run or not any(i.mode != "skip" for i in plan.items):
        return
    out, report = vst3.convert(plan, a.out)
    versions.write_notes(out, parent_path=a.set, extra_lines=report)
    print("\n" + "\n".join(report))
    print(f"\nSaved {out}\nOpen it in Live and check each converted plug-in before you keep working in it.")


def cmd_startup(action):
    if sys.platform != "win32":
        raise SystemExit("Windows only.")
    startup = os.path.join(os.environ["APPDATA"], r"Microsoft\Windows\Start Menu\Programs\Startup")
    target = os.path.join(startup, "Live Versions.cmd")
    if action == "remove":
        if os.path.exists(target):
            os.remove(target)
        print("Removed from startup.")
        return
    pyw = os.path.join(os.path.dirname(sys.executable), "pythonw.exe")
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    with open(target, "w", encoding="utf-8") as f:
        f.write(f'@echo off\r\ncd /d "{here}"\r\nstart "" "{pyw}" -m liveversions run\r\n')
    print(f"Added to startup: {target}")


if __name__ == "__main__":
    main()
