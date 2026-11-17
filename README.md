# Live Versions

Version management for Ableton Live sets, plus a VST2/AU → VST3 converter.
One file, `LiveVersions.exe` (about 6 MB). No installer, nothing else to install.

## Using it

1. Put `LiveVersions.exe` anywhere (e.g. `Documents\Live Versions\`) and double-click it.
   An icon appears in the system tray (it may be under the **^** arrow; drag it out to keep it visible).
2. In Live, press **Ctrl+Alt+S**. The set is saved as the next version (`Song.als` → `Song v_2.als`,
   `Song v_6.als` → `Song v_7.als`) and Live keeps working in the new file. A small pop-up confirms
   it and says what changed.
3. Press **Ctrl+Alt+V** (or click the tray icon) to open the **version browser**.

Right-click the tray icon for: version browser, Plug-in report, Convert a set to VST3, Settings,
**Start with Windows**, and **Quit** (which also closes any open browser, converter or settings window).

If you're in an older version (v_3 while v_6 exists), the shortcut saves **v_7**, so nothing is
ever overwritten. Running the exe a second time just opens the browser.

### Version name formats

Recognised: `Song v_3`, `Song_3` (Live Enhancement Suite), `Song v3`, `Song v003`, `Song T03`,
`Song Take 3`, and the words `ver`, `version`, `rev`, `r`. Dates (`2026-11-13`) and names like
`Kick 2` aren't versions.

Text after the number belongs to that version: `Song v054 MASTER`, `Song T58 HD`, `Song T50b`
and `Song v051 Bass Update_3` are v054, T58, T50 and v051 of `Song`. They're listed with the
other versions, saving from one continues the main numbering (`Song v055`), and its change notes
compare it with the plain version of the same number (`v054`) when there is one.

The format of each new version is chosen in this order:

1. the format of the set you're in (`Song T02` → `Song T03`, `Song v009` → `Song v010`);
2. for a set without a number: the format of its other versions;
3. else the format of the most recently saved version in the same folder;
4. else your default from **Settings → New version names** (presets, or your own pattern:
   the text before the number, then one `#` per digit, e.g. ` v_#` or ` T##`).

Numbers continue across formats. Turn off **Keep the format a set already uses** to always use
your default.

> Live Enhancement Suite also saves versions on Ctrl+Alt+S. Turn that off in LES, or pick another
> shortcut in Settings. The app warns you once if it sees LES running.

## Change notes

Each version gets a text file in `<Project>/Version Notes/<set name>.txt` (or next to the set,
see Settings):

```
Summary: +13 tracks

Tracks added
  + Group "10 Drums"
  + Audio "AVG Rim" in group "10 Drums" (+ Pro-Q 3 [VST3], Audio Effect Rack 'Cuts Rack')
Instruments changed
  "Lead": Wavetable → Serum 2 [VST3]
Devices added / removed
  "Bass": + Saturator, − EQ Eight
Clips & notes
  "Kick": +1 arrangement clip, +56 notes
```

Reported: tracks added/removed/renamed, instruments (including inside instrument racks), devices
added/removed (a rack loaded from a preset counts as one device), grouping, new samples, tempo,
key, arrangement length, locators, clip and note counts.
Ignored: knob tweaks, automation curves, small mixer moves, colours, and track renumbering
(`12 Bass` → `25 Bass` isn't a rename).

## Version browser

- **Follow Live**: shows whichever set is open in Live, and refreshes when a new version appears.
- **Open in Live** (or double-click), **Compare** (Ctrl+click two versions), **Regenerate notes**,
  **Show in folder**, **Plug-in report**, **Convert to VST3…**
- **Your notes for this version**: free text, stored in the same .txt and kept when notes are regenerated.
- Drop any `.als` on the window (or on the exe) to browse its versions.

## VST2 / AU → VST3

From the browser (**Convert to VST3…**) or the tray menu. The original set is never changed; the
result is saved as the next version with a report in its notes. Per plug-in:

| Shown as | When | Result |
|---|---|---|
| Settings kept | The VST3 declares it replaces the VST2 (Valhalla, Soundtoys, Kontakt, Dada Life, Kick 2, …) | Lossless: same data Live/Cubase hand over |
| Old data passed as-is | Same plug-in name, JUCE-made | Usually works; check it in Live |
| Defaults + old plug-in kept | Same name, unrelated IDs (Spire, Omnisphere, …) | VST3 with defaults, old plug-in kept next to it switched off so you can copy settings |
| Not converted | No matching VST3 installed, or a shell plug-in (Waves) | Unchanged |

You can change the choice per plug-in before converting.

**Plug-in report:** to see what converting would do without changing anything, use **Plug-in report**
in the browser (or *Plug-in report for a set…* in the tray menu), or the **Report** tab of the converter.
It lists every VST2/AU plug-in with what would happen to it, the same lines the conversion writes
afterwards, plus a summary (how many keep their settings, how much automation is kept). It updates
when you change a choice on the Plug-ins tab, and can be copied or saved as a text file.

**Automation, rack macros and MIDI mappings** move to the VST3 when its parameter IDs are known.
They're learned from any set where you've already used that VST3: automatically, from every set
the app reads, or in bulk with **Learn parameter IDs from a folder…**. If an ID isn't known, that
automation is removed rather than left driving the wrong control, and the report lists it.

Installed VST3s are read from Live's own plug-in database, so rescan plug-ins in Live first if you
just installed one. Audio Unit conversion (sets made on a Mac) is supported but untested.

**Older sets:** VST3 support arrived in **Live 10.1**. A set last saved with Live 9 or Live 10.0
can't hold VST3 plug-ins, so it isn't converted (the report still shows what would happen). Open it
in your current Live, save it (Ctrl+Alt+S saves it as the next version), then convert that version.

## Command line

The same exe has a command line (run it from a terminal):

```
LiveVersions notes <set.als>...         (re)write change notes
LiveVersions diff <old.als> <new.als>   print what changed
LiveVersions versions <set.als|folder>  list versions and what the next one will be called
LiveVersions report <set.als>           plug-in report: what converting would do (changes nothing)
LiveVersions vst3 <set.als> [--dry-run] [--out <path>] [--mode "<plugin>=<mode>"]
LiveVersions learn <folder>...          learn VST3 parameter IDs from sets
```

## Where things are

- Settings and learned plug-in data: `%APPDATA%\LiveVersions`
- Cache of parsed sets (safe to delete): `%LOCALAPPDATA%\LiveVersions\cache`
- "Start with Windows" adds `Live Versions` under `HKCU\...\CurrentVersion\Run`

## Building

Needs Rust and Visual Studio (or its Build Tools) with the MSVC linker and the Windows SDK.
No C compiler is needed.

```
.\build.ps1 --release        # -> target\release\LiveVersions.exe
.\build.ps1 test             # unit tests
.\build.ps1 test-all         # also drives a real Save dialog (takes focus for a few seconds)
```

`assets/make_icon.py` regenerates the icon.

Source: `src/als.rs` (read sets, snapshot), `changes.rs` (diff), `versions.rs` (v_N naming,
notes), `vst3.rs` (converter), `sqlite.rs` (reads Live's plug-in database), `gui.rs` (windows),
`win/` (tray, hotkeys, pop-up, Save As automation).
