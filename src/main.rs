#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Live Versions — version manager for Ableton Live sets.
//!
//!   LiveVersions.exe                  tray icon + hotkeys (Windows)
//!   LiveVersions.exe <set.als|folder> version browser for that set
//!   LiveVersions.exe --browse [set]   version browser
//!   LiveVersions.exe --convert [set]  VST2/AU -> VST3 converter
//!   LiveVersions.exe --report [set]   plug-in report (what converting would do)
//!   LiveVersions.exe --settings       settings
//!   LiveVersions.exe notes|diff|vst3|learn …   command line (see `help`)
mod als;
mod changes;
mod config;
mod gui;
mod sqlite;
mod versions;
mod vst3;
#[cfg(windows)]
mod win;

use std::path::PathBuf;

const HELP: &str = "Live Versions — version manager for Ableton Live sets

Usage:
  LiveVersions                          start in the tray (hotkeys: save version, browser)
  LiveVersions <set.als | folder>       open the version browser
  LiveVersions notes <set.als>...       (re)write change notes
  LiveVersions diff <old.als> <new.als> print what changed
  LiveVersions versions <set.als | folder>  list versions and the next version's name
  LiveVersions report <set.als>         which plug-ins can be converted to VST3 (changes nothing)
  LiveVersions vst3 <set.als> [--dry-run] [--out <path>] [--mode <plugin>=<mode>]...
        modes: vst2state raw sidebyside duplicate default skip
  LiveVersions learn <folder>...        learn VST3 parameter IDs from sets

Copyright (C) 2026 MusicProWorkshop. This program comes with ABSOLUTELY NO WARRANTY.
It is free software under the GNU GPL v3 or later; you may redistribute it under its terms.
Source and license: https://github.com/MusicProWorkshop/AbletonLiveVersions";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);
    match first {
        None | Some("--startup") => run_tray(first == Some("--startup")),
        Some("--browse") => gui::run(gui::Start::Browser(args.get(1).map(PathBuf::from))),
        Some("--convert") => gui::run(gui::Start::Convert(args.get(1).map(PathBuf::from))),
        Some("--report") => gui::run(gui::Start::Report(args.get(1).map(PathBuf::from))),
        Some("--settings") => gui::run(gui::Start::Settings),
        Some(cmd @ ("notes" | "diff" | "vst3" | "report" | "learn" | "versions" | "help" | "--help" | "-h" | "/?")) => {
            #[cfg(windows)]
            win::attach_console();
            let code = match cli(cmd, &args[1..]) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            };
            std::process::exit(code);
        }
        // Dropped on the exe or "Open with": browse that set.
        Some(p) => gui::run(gui::Start::Browser(Some(PathBuf::from(p)))),
    }
}

fn run_tray(quiet: bool) {
    #[cfg(windows)]
    {
        if !win::single_instance("Local\\LiveVersionsTray") {
            // Already running: starting it again opens the browser instead.
            win::tray::spawn_browser_for(win::live::current_set_path());
            return;
        }
        win::tray::run(quiet);
    }
    #[cfg(not(windows))]
    {
        let _ = quiet;
        gui::run(gui::Start::Browser(None));
    }
}

fn cli(cmd: &str, rest: &[String]) -> Result<(), String> {
    match cmd {
        "notes" => {
            if rest.is_empty() {
                return Err("notes needs at least one .als file".into());
            }
            for p in rest {
                let p = PathBuf::from(p);
                let s = versions::write_notes(&p, None, None, None)?;
                println!("{}: {s}\n  -> {}", p.file_name().unwrap_or_default().to_string_lossy(), versions::notes_path(&p).display());
            }
        }
        "diff" => {
            let [a, b] = rest else { return Err("diff needs two .als files".into()) };
            let ch = changes::diff(&als::snapshot(a.as_ref())?, &als::snapshot(b.as_ref())?);
            println!("Summary: {}\n", changes::summary_line(&ch));
            println!("{}", changes::format_lines(&ch).join("\n"));
        }
        "vst3" => cli_vst3(rest)?,
        "versions" => {
            let p = PathBuf::from(rest.first().ok_or("versions needs a .als file or a folder")?);
            let fams = if p.is_dir() {
                versions::families_in(&p)
            } else {
                let fam = versions::family(&p);
                vec![(versions::parse(&p).base, fam)]
            };
            for (base, vs) in fams {
                println!("{base}");
                for v in &vs {
                    println!("  {:10} {}", v.label(), v.path.file_name().unwrap_or_default().to_string_lossy());
                }
                if let Some(last) = vs.last() {
                    let next = versions::next_version_path(&last.path);
                    println!("  next:      {}", next.file_name().unwrap_or_default().to_string_lossy());
                }
            }
        }
        "report" => {
            let mut r = vec!["--dry-run".to_string()];
            r.extend(rest.iter().cloned());
            cli_vst3(&r)?
        }
        "learn" => {
            let paths: Vec<PathBuf> = rest.iter().map(PathBuf::from).collect();
            let n = vst3::learn_paths(&paths, |i, t| {
                if i % 25 == 0 || i == t {
                    println!("{i}/{t}");
                }
            });
            println!("Learned {n} new parameter IDs.");
        }
        _ => println!("{HELP}"),
    }
    Ok(())
}

fn cli_vst3(rest: &[String]) -> Result<(), String> {
    let mut set = None;
    let (mut dry, mut out, mut overrides) = (false, None, Vec::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry = true,
            "--out" => out = it.next().map(PathBuf::from),
            "--mode" => {
                let m = it.next().ok_or("--mode needs <plugin>=<mode>")?;
                let (name, mode) = m.rsplit_once('=').ok_or("--mode needs <plugin>=<mode>")?;
                let mode = vst3::Mode::from_name(mode).ok_or(format!("unknown mode '{mode}'"))?;
                overrides.push((name.to_lowercase(), mode));
            }
            p => set = Some(PathBuf::from(p)),
        }
    }
    let set = set.ok_or("needs a .als file")?;
    let mut plan = vst3::analyze(&set, None)?;
    for item in plan.items.iter_mut() {
        if let Some((_, m)) = overrides.iter().find(|(n, _)| *n == item.src.name.to_lowercase()) {
            if item.modes().contains(m) {
                item.mode = *m;
            }
        }
    }
    if dry || plan.items.iter().all(|i| i.mode == vst3::Mode::Skip) {
        // Same lines the conversion reports, without changing anything.
        println!("{}", vst3::preview_report(&plan).join("\n"));
        return Ok(());
    }
    let (out, report) = vst3::convert(&plan, out)?;
    versions::write_notes(&out, Some(&set), Some(report.clone()), None)?;
    println!("\n{}\n\nSaved {}\nOpen it in Live and check each converted plug-in before you keep working in it.", report.join("\n"), out.display());
    Ok(())
}
