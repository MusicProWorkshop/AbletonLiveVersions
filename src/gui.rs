//! The windows: version browser, VST3 converter and settings (egui).
use eframe::egui::{self, Color32, RichText, Vec2};
use egui_extras::{Column, TableBuilder};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sets::{self, SetEntry};
use crate::{als, changes, config, versions, vst3};

const BG: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
const PANEL: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
const FIELD: Color32 = Color32::from_rgb(0x15, 0x15, 0x15);
const FG: Color32 = Color32::from_rgb(0xdd, 0xdd, 0xdd);
const DIM: Color32 = Color32::from_rgb(0x8f, 0x8f, 0x8f);
const ACCENT: Color32 = Color32::from_rgb(0xff, 0x9b, 0x45);
const SEL: Color32 = Color32::from_rgb(0x36, 0x50, 0x6e);
const GOOD: Color32 = Color32::from_rgb(0x8f, 0xd1, 0x8f);
const WARN: Color32 = Color32::from_rgb(0xf0, 0xc6, 0x74);
const BAD: Color32 = Color32::from_rgb(0xe8, 0x8b, 0x8b);

pub enum Start {
    Browser(Option<PathBuf>),
    Convert(Option<PathBuf>),
    /// The converter, opened on its Report tab.
    Report(Option<PathBuf>),
    Settings,
    /// Live Sets: the latest version of each set.
    Sets,
}

pub fn run(start: Start) {
    let start = match start {
        Start::Convert(None) => match pick_set() {
            Some(p) => Start::Convert(Some(p)),
            None => return,
        },
        Start::Report(None) => match pick_set() {
            Some(p) => Start::Report(Some(p)),
            None => return,
        },
        s => s,
    };
    let (title, size) = match &start {
        Start::Settings => ("Live Versions — Settings", [720.0, 600.0]),
        Start::Convert(_) | Start::Report(_) => ("Live Versions — Convert to VST3", [1120.0, 640.0]),
        Start::Browser(_) => ("Live Versions", [1200.0, 720.0]),
        Start::Sets => ("Live Versions — Live Sets", [1200.0, 720.0]),
    };
    let icon = egui::IconData { rgba: include_bytes!("../assets/icon64.rgba").to_vec(), width: 64, height: 64 };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(size)
            .with_min_inner_size([520.0, 400.0])
            .with_icon(Arc::new(icon))
            .with_drag_and_drop(true),
        centered: true,
        ..Default::default()
    };
    let _ = eframe::run_native(
        title,
        opts,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(App::new(start, cc.egui_ctx.clone())))
        }),
    );
}

fn pick_set() -> Option<PathBuf> {
    rfd::FileDialog::new().set_title("Choose an Ableton Live set").add_filter("Live Set", &["als"]).pick_file()
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let load = |f: &str| std::fs::read(Path::new(&windir).join("Fonts").join(f)).ok();
    if let Some(b) = load("segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(b));
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    if let Some(b) = load("consola.ttf") {
        fonts.font_data.insert("consolas".into(), egui::FontData::from_owned(b));
        fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "consolas".into());
    }
    if let Some(b) = load("seguisym.ttf") {
        fonts.font_data.insert("symbols".into(), egui::FontData::from_owned(b));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push("symbols".into());
        }
    }
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = Color32::from_rgb(0x1a, 0x1a, 0x1a);
    v.override_text_color = Some(FG);
    v.selection.bg_fill = SEL;
    v.selection.stroke.color = Color32::WHITE;
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(0x3a, 0x3a, 0x3a);
    v.widgets.inactive.weak_bg_fill = PANEL;
    v.widgets.inactive.bg_fill = PANEL;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3a, 0x3a, 0x3a);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x44, 0x44, 0x44);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.rounding = egui::Rounding::same(4.0);
    }
    ctx.set_visuals(v);
    let mut st = (*ctx.style()).clone();
    st.spacing.item_spacing = Vec2::new(8.0, 6.0);
    st.spacing.button_padding = Vec2::new(12.0, 5.0);
    use egui::{FontFamily as F, FontId, TextStyle as T};
    st.text_styles = [
        (T::Heading, FontId::new(22.0, F::Proportional)),
        (T::Body, FontId::new(14.5, F::Proportional)),
        (T::Button, FontId::new(14.5, F::Proportional)),
        (T::Monospace, FontId::new(13.5, F::Monospace)),
        (T::Small, FontId::new(12.5, F::Proportional)),
    ]
    .into();
    ctx.set_style(st);
}

fn accent_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(Color32::from_rgb(0x1a, 0x1a, 0x1a)).strong()).fill(ACCENT)
}

fn open_in_live(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

fn reveal(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Colored rendering of a notes / compare / report text.
fn show_text(ui: &mut egui::Ui, text: &str) {
    for line in text.lines() {
        let s = line.trim();
        if !s.is_empty() && s.chars().all(|c| c == '=') {
            continue; // title underline is for the .txt file only
        }
        let rt = RichText::new(line).monospace();
        let rt = if s.starts_with("Summary:") {
            rt.color(Color32::WHITE).strong()
        } else if s.starts_with("Saved:") || s.starts_with("Compared with:") || s.starts_with("Ableton Live") || s.starts_with('·') {
            rt.color(DIM)
        } else if s.starts_with("+ ") || s.starts_with('✓') {
            rt.color(GOOD)
        } else if s.starts_with("− ") {
            rt.color(BAD)
        } else if s.starts_with("lost automation") {
            rt.color(WARN)
        } else if !s.is_empty() && !line.starts_with(' ') {
            rt.color(ACCENT)
        } else {
            rt
        };
        ui.label(rt);
    }
}

// ----------------------------------------------------------------------------- app

enum Msg {
    RowReady(PathBuf),
    Status(String),
    Compare(String),
    Plan(Result<vst3::Plan, String>),
    Converted(Result<(PathBuf, Vec<String>), String>),
    Learned(usize),
    Recent(Vec<SetEntry>),
    /// (scan id, folders looked at so far)
    ScanProgress(u64, usize),
    Scanned(u64, Vec<SetEntry>),
}

#[derive(PartialEq)]
enum Screen {
    Browser,
    Convert,
    Settings,
    Sets,
}

#[derive(PartialEq, Clone, Copy)]
enum SetsTab {
    Recent,
    Folders,
}

struct SetsUi {
    tab: SetsTab,
    recent: Option<Vec<SetEntry>>,
    scanned: Option<Vec<SetEntry>>,
    /// Folders looked at by the scan that's running, if one is.
    scanning: Option<usize>,
    scan_id: u64,
    cancel: Arc<AtomicBool>,
    folders: Vec<PathBuf>,
    filter: String,
    /// One set per project folder (the most recently saved one).
    per_project: bool,
    /// Selected set (its key).
    sel: Option<String>,
}

struct Row {
    path: PathBuf,
    label: String,
    saved: String,
    tracks: String,
    summary: String,
    pending: bool,
    cloud: bool,
}

struct ConvertUi {
    path: PathBuf,
    plan: Option<vst3::Plan>,
    error: Option<String>,
    sel: usize,
    busy: Option<String>,
    result: Option<(PathBuf, Vec<String>)>,
    /// Showing the "Report" tab (what converting would do) instead of the table.
    report_view: bool,
    /// Small status next to the report buttons ("Report copied").
    busy_note: Option<String>,
}

struct SettingsUi {
    cfg: config::Config,
    startup: bool,
    message: String,
}

struct App {
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    screen: Screen,
    only_screen: bool,
    cfg: config::Config,
    // browser
    folder: Option<PathBuf>,
    base: String,
    families: Vec<(String, usize)>,
    rows: Vec<Row>,
    sel: Vec<usize>,
    details: String,
    note: String,
    note_orig: String,
    status: String,
    follow_live: bool,
    last_live: Option<PathBuf>,
    current_open: Option<PathBuf>,
    last_poll: Instant,
    dir_sig: String,
    job: Arc<AtomicU64>,
    // other screens
    conv: Option<ConvertUi>,
    settings: Option<SettingsUi>,
    sets: Option<SetsUi>,
    /// Settings was opened from Live Sets: go back there when it closes.
    settings_from_sets: bool,
    /// Live Sets is showing because no set was open in Live: switch to the
    /// browser when one is (with Follow Live on).
    sets_until_live: bool,
    title: String,
}

fn dir_signature(folder: &Path) -> String {
    let mut v: Vec<String> = std::fs::read_dir(folder)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().map(|x| x.eq_ignore_ascii_case("als")).unwrap_or(false))
                .map(|e| {
                    let m = e.metadata().and_then(|m| m.modified()).ok();
                    format!("{}|{:?}", e.file_name().to_string_lossy(), m)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.join(";")
}

fn live_set() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::win::live::current_set_path()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

impl App {
    fn new(start: Start, ctx: egui::Context) -> App {
        let (tx, rx) = channel();
        let mut app = App {
            ctx,
            tx,
            rx,
            screen: Screen::Browser,
            only_screen: false,
            cfg: config::load(),
            folder: None,
            base: String::new(),
            families: vec![],
            rows: vec![],
            sel: vec![],
            details: String::new(),
            note: String::new(),
            note_orig: String::new(),
            status: String::new(),
            follow_live: true,
            last_live: None,
            current_open: None,
            last_poll: Instant::now(),
            dir_sig: String::new(),
            job: Arc::new(AtomicU64::new(0)),
            conv: None,
            settings: None,
            sets: None,
            settings_from_sets: false,
            sets_until_live: false,
            title: String::new(),
        };
        match start {
            Start::Browser(p) => {
                app.last_live = live_set();
                app.current_open = app.last_live.clone();
                match p.or_else(|| app.last_live.clone()) {
                    Some(p) => app.load(&p),
                    // No set given and none open in Live (or Live isn't running): the sets list,
                    // until a set is opened in Live.
                    None => {
                        app.open_sets();
                        app.sets_until_live = true;
                    }
                }
            }
            Start::Convert(Some(p)) => {
                app.only_screen = true;
                app.open_convert(p, false);
            }
            Start::Report(Some(p)) => {
                app.only_screen = true;
                app.open_convert(p, true);
            }
            Start::Convert(None) | Start::Report(None) => {}
            Start::Settings => {
                app.only_screen = true;
                app.open_settings();
            }
            Start::Sets => {
                app.last_live = live_set();
                app.current_open = app.last_live.clone();
                app.open_sets();
            }
        }
        app
    }

    fn send(&self) -> (Sender<Msg>, egui::Context) {
        (self.tx.clone(), self.ctx.clone())
    }

    // ---- browser data
    fn load(&mut self, path: &Path) {
        let mut path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if path.is_dir() {
            let fams = versions::families_in(&path);
            let newest = fams.iter().max_by_key(|(_, vs)| vs.iter().filter_map(|v| std::fs::metadata(&v.path).and_then(|m| m.modified()).ok()).max());
            match newest {
                Some((_, vs)) => path = vs.last().unwrap().path.clone(),
                None => {
                    self.status = "No Live sets in that folder.".into();
                    return;
                }
            }
        }
        if !path.exists() {
            self.status = format!("{} doesn't exist.", path.display());
            return;
        }
        self.folder = path.parent().map(Path::to_path_buf);
        self.base = versions::parse(&path).base.trim().to_string();
        self.sel.clear();
        self.refresh();
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.select_only(i);
        }
    }

    fn refresh(&mut self) {
        let Some(folder) = self.folder.clone() else { return };
        let keep: Vec<PathBuf> = self.sel.iter().filter_map(|i| self.rows.get(*i).map(|r| r.path.clone())).collect();
        self.dir_sig = dir_signature(&folder);
        let fams = versions::families_in(&folder);
        self.families = fams.iter().map(|(b, v)| (b.clone(), v.len())).collect();
        let family = fams.into_iter().find(|(b, _)| *b == self.base).map(|(_, v)| v).unwrap_or_default();
        self.current_open = live_set();
        self.rows = family
            .iter()
            .rev()
            .map(|v| {
                let n = versions::read_notes(&v.path);
                let summary = versions::summary_from_notes(&n);
                let cloud = als::is_cloud_only(&v.path);
                let saved: chrono::DateTime<chrono::Local> =
                    std::fs::metadata(&v.path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH).into();
                Row {
                    path: v.path.clone(),
                    label: v.label(),
                    saved: saved.format("%d %b %Y  %H:%M").to_string(),
                    tracks: versions::tracks_from_notes(&n).unwrap_or_default(),
                    pending: summary.is_none(),
                    summary: summary.unwrap_or_else(|| {
                        if cloud { "only in the cloud — select it and click Regenerate notes".into() } else { "reading…".into() }
                    }),
                    cloud,
                }
            })
            .collect();
        self.sel = keep.iter().filter_map(|p| self.rows.iter().position(|r| &r.path == p)).collect();
        if self.sel.is_empty() && !self.rows.is_empty() {
            self.sel = vec![0];
        }
        self.update_details();
        self.status = format!("{} versions", self.rows.len());
        if self.cfg.auto_write_missing_notes {
            let todo: Vec<PathBuf> = self.rows.iter().rev().filter(|r| r.pending && !r.cloud).map(|r| r.path.clone()).collect();
            self.write_notes_bg(todo);
        }
    }

    fn write_notes_bg(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let job = self.job.fetch_add(1, Ordering::SeqCst) + 1;
        let jobref = self.job.clone();
        let (tx, ctx) = self.send();
        std::thread::spawn(move || {
            let n = paths.len();
            for (i, p) in paths.into_iter().enumerate() {
                if jobref.load(Ordering::SeqCst) != job {
                    return;
                }
                let _ = tx.send(Msg::Status(format!("Writing change notes {}/{n}…", i + 1)));
                ctx.request_repaint();
                match versions::write_notes(&p, None, None, None) {
                    Ok(_) => {
                        let _ = tx.send(Msg::RowReady(p));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Status(e));
                    }
                }
                ctx.request_repaint();
            }
            let _ = tx.send(Msg::Status(String::new()));
            ctx.request_repaint();
        });
    }

    fn select_only(&mut self, i: usize) {
        self.sel = vec![i];
        self.update_details();
    }

    fn update_details(&mut self) {
        self.note.clear();
        self.note_orig.clear();
        match self.sel.as_slice() {
            [i] => {
                if let Some(r) = self.rows.get(*i) {
                    let n = versions::read_notes(&r.path);
                    self.details = if n.text.is_empty() { "No change notes for this version yet.\n\nClick “Regenerate notes”.".into() } else { n.text };
                    self.note = n.user.clone();
                    self.note_orig = n.user;
                }
            }
            [_, _] => self.details = "Two versions selected.\n\nClick “Compare” to see what changed between them.".into(),
            _ => self.details.clear(),
        }
    }

    fn poll(&mut self) {
        if self.last_poll.elapsed() < Duration::from_millis(1200) {
            return;
        }
        self.last_poll = Instant::now();
        let live = live_set();
        if live != self.last_live {
            self.last_live = live.clone();
            if let Some(p) = &live {
                // Also recorded by the tray; this covers running the browser on its own.
                sets::record_opened(p);
            }
            if self.screen == Screen::Sets {
                if let (true, true, Some(p)) = (self.sets_until_live, self.follow_live, &live) {
                    self.sets_until_live = false;
                    self.screen = Screen::Browser;
                    self.load(p);
                    return;
                }
                self.current_open = live;
                self.reload_recent();
                return;
            }
            if let (true, Some(p)) = (self.follow_live, &live) {
                let same_family = self.folder.as_deref() == p.parent() && versions::parse(p).base.trim() == self.base;
                if !same_family && self.screen == Screen::Browser {
                    self.load(p);
                    return;
                }
            }
            self.current_open = live;
        }
        if self.screen != Screen::Browser {
            return;
        }
        if let Some(f) = &self.folder {
            if dir_signature(f) != self.dir_sig {
                self.refresh();
            }
        }
    }

    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::RowReady(p) => {
                    if let Some(i) = self.rows.iter().position(|r| r.path == p) {
                        let n = versions::read_notes(&p);
                        let r = &mut self.rows[i];
                        r.summary = versions::summary_from_notes(&n).unwrap_or_default();
                        r.tracks = versions::tracks_from_notes(&n).unwrap_or_default();
                        r.pending = false;
                        if self.sel == [i] {
                            self.update_details();
                        }
                    }
                }
                Msg::Status(s) => self.status = if s.is_empty() { format!("{} versions", self.rows.len()) } else { s },
                Msg::Compare(t) => self.details = t,
                Msg::Plan(p) => {
                    if let Some(c) = self.conv.as_mut() {
                        c.busy = None;
                        match p {
                            Ok(plan) => c.plan = Some(plan),
                            Err(e) => c.error = Some(e),
                        }
                    }
                }
                Msg::Converted(r) => {
                    if let Some(c) = self.conv.as_mut() {
                        c.busy = None;
                        match r {
                            Ok(res) => c.result = Some(res),
                            Err(e) => c.error = Some(e),
                        }
                    }
                }
                Msg::Learned(n) => {
                    self.status = format!("Learned {n} parameter IDs");
                    if let Some((p, rv)) = self.conv.as_ref().map(|c| (c.path.clone(), c.report_view)) {
                        self.open_convert(p, rv);
                    }
                }
                Msg::Recent(v) => {
                    if let Some(s) = self.sets.as_mut() {
                        s.recent = Some(v);
                    }
                }
                Msg::ScanProgress(id, n) => {
                    if let Some(s) = self.sets.as_mut().filter(|s| s.scan_id == id) {
                        s.scanning = Some(n);
                    }
                }
                Msg::Scanned(id, v) => {
                    if let Some(s) = self.sets.as_mut().filter(|s| s.scan_id == id) {
                        s.scanning = None;
                        s.scanned = Some(v);
                    }
                }
            }
        }
    }

    // ---- browser UI
    fn browser_ui(&mut self, ctx: &egui::Context) {
        let mut action: Option<&str> = None;
        egui::TopBottomPanel::top("top").frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 18.0, right: 18.0, top: 14.0, bottom: 8.0 })).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new(if self.base.is_empty() { "No set loaded" } else { &self.base }).strong());
                if self.families.len() > 1 {
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("family").selected_text("Other sets in this folder").width(220.0).show_ui(ui, |ui| {
                        for (b, n) in &self.families {
                            if ui.selectable_label(*b == self.base, format!("{b}  ({n})")).clicked() {
                                chosen = Some(b.clone());
                            }
                        }
                    });
                    if let Some(b) = chosen {
                        self.base = b;
                        self.sel.clear();
                        self.refresh();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        action = Some("settings");
                    }
                    if ui.button("Live Sets").on_hover_text("The latest version of each set you've opened, or of every set in your folders").clicked() {
                        action = Some("sets");
                    }
                    if ui.button("Open set or folder…").clicked() {
                        action = Some("browse");
                    }
                    if cfg!(windows) {
                        ui.checkbox(&mut self.follow_live, "Follow Live").on_hover_text("Show the set that's open in Live");
                    }
                });
            });
            if let Some(f) = &self.folder {
                ui.label(RichText::new(f.display().to_string()).color(DIM).small());
            }
        });

        egui::TopBottomPanel::bottom("bottom").frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 18.0, right: 18.0, top: 8.0, bottom: 14.0 })).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let one = self.sel.len() == 1;
                if ui.add_enabled(one, accent_button("Open in Live")).clicked() {
                    action = Some("open");
                }
                if ui.add_enabled(self.sel.len() == 2, egui::Button::new("Compare")).on_hover_text("Ctrl+click two versions").clicked() {
                    action = Some("compare");
                }
                if ui.add_enabled(one, egui::Button::new("Regenerate notes")).clicked() {
                    action = Some("regen");
                }
                if ui.add_enabled(one, egui::Button::new("Show in folder")).clicked() {
                    action = Some("reveal");
                }
                if ui.add_enabled(one, egui::Button::new("Plug-in report")).on_hover_text("Which plug-ins can be converted to VST3, without changing anything").clicked() {
                    action = Some("report");
                }
                if ui.add_enabled(one, egui::Button::new("Convert to VST3…")).clicked() {
                    action = Some("convert");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&self.status).color(DIM));
                });
            });
        });

        egui::SidePanel::right("details")
            .resizable(true)
            .default_width(470.0)
            .min_width(320.0)
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 8.0, right: 18.0, top: 4.0, bottom: 4.0 }))
            .show(ctx, |ui| {
                // Details fill everything above the notes box.
                let details_h = (ui.available_height() - 190.0).max(120.0);
                egui::Frame::none().fill(FIELD).rounding(4.0).inner_margin(12.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("det").auto_shrink([false, false]).max_height(details_h - 24.0).show(ui, |ui| {
                        show_text(ui, &self.details);
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Your notes for this version").color(DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(self.sel.len() == 1 && self.note != self.note_orig, egui::Button::new("Save note")).clicked() {
                            action = Some("savenote");
                        }
                    });
                });
                ui.add_enabled(
                    self.sel.len() == 1,
                    egui::TextEdit::multiline(&mut self.note).desired_width(f32::INFINITY).desired_rows(5).hint_text("What did you try in this version?"),
                );
            });

        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 18.0, right: 8.0, top: 4.0, bottom: 4.0 })).show(ctx, |ui| {
            if self.rows.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Open a Live set to see its versions.").color(DIM));
                    ui.add_space(8.0);
                    if ui.add(accent_button("Open set or folder…")).clicked() {
                        action = Some("browse");
                    }
                    if ui.button("Live Sets").clicked() {
                        action = Some("sets");
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("You can also drop a .als file here.").color(DIM).small());
                });
                return;
            }
            let mut clicked: Option<(usize, bool)> = None;
            let mut double: Option<usize> = None;
            let open_cur = self.current_open.clone();
            egui::Frame::none().fill(FIELD).rounding(4.0).show(ui, |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(160.0).clip(true))
                    .column(Column::exact(140.0))
                    .column(Column::exact(56.0))
                    .column(Column::remainder().clip(true))
                    .header(28.0, |mut h| {
                        for t in ["  Version", "Saved", "Tracks", "What changed"] {
                            h.col(|ui| {
                                ui.label(RichText::new(t).color(DIM).small());
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(28.0, self.rows.len(), |mut row| {
                            let i = row.index();
                            let r = &self.rows[i];
                            row.set_selected(self.sel.contains(&i));
                            let is_open = open_cur.as_ref() == Some(&r.path);
                            let col = if r.pending { DIM } else { FG };
                            row.col(|ui| {
                                let t = if is_open { format!("  ▶ {}", r.label) } else { format!("  {}", r.label) };
                                ui.label(RichText::new(t).color(if is_open { ACCENT } else { FG }).strong());
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(&r.saved).color(DIM));
                            });
                            row.col(|ui| {
                                ui.label(&r.tracks);
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(&r.summary).color(col)).on_hover_text(&r.summary);
                            });
                            let resp = row.response();
                            if is_open {
                                resp.clone().on_hover_text("Open in Live right now");
                            }
                            if resp.double_clicked() {
                                double = Some(i);
                            } else if resp.clicked() {
                                clicked = Some((i, ui_ctrl(&resp)));
                            }
                        });
                    });
            });
            if let Some((i, ctrl)) = clicked {
                if ctrl {
                    if let Some(pos) = self.sel.iter().position(|x| *x == i) {
                        self.sel.remove(pos);
                    } else {
                        self.sel.push(i);
                        if self.sel.len() > 2 {
                            self.sel.remove(0);
                        }
                    }
                    self.update_details();
                } else {
                    self.select_only(i);
                }
            }
            if let Some(i) = double {
                self.select_only(i);
                action = Some("open");
            }
        });

        // keyboard
        if self.screen == Screen::Browser && !ctx.wants_keyboard_input() && !self.rows.is_empty() {
            let cur = self.sel.first().copied().unwrap_or(0);
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                self.select_only((cur + 1).min(self.rows.len() - 1));
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                self.select_only(cur.saturating_sub(1));
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && self.sel.len() == 1 {
                action = Some("open");
            }
        }

        let sel_path = |s: &Self| s.sel.first().and_then(|i| s.rows.get(*i)).map(|r| r.path.clone());
        match action {
            Some("browse") => {
                if let Some(p) = pick_set() {
                    self.follow_live = false;
                    self.load(&p);
                }
            }
            Some("settings") => self.open_settings(),
            Some("sets") => self.open_sets(),
            Some("open") => {
                if let Some(p) = sel_path(self) {
                    open_in_live(&p);
                    self.status = format!("Opening {} in Live…", stem(&p));
                }
            }
            Some("reveal") => {
                if let Some(p) = sel_path(self) {
                    reveal(&p);
                }
            }
            Some("regen") => {
                if let Some(p) = sel_path(self) {
                    if let Some(i) = self.rows.iter().position(|r| r.path == p) {
                        self.rows[i].summary = "reading…".into();
                        self.rows[i].pending = true;
                    }
                    self.write_notes_bg(vec![p]);
                }
            }
            Some("savenote") => {
                if let Some(p) = sel_path(self) {
                    match versions::save_user_note(&p, &self.note) {
                        Ok(()) => {
                            self.note_orig = self.note.clone();
                            self.status = "Note saved".into();
                        }
                        Err(e) => self.status = e,
                    }
                }
            }
            Some("compare") => {
                let mut ps: Vec<PathBuf> = self.sel.iter().filter_map(|i| self.rows.get(*i).map(|r| r.path.clone())).collect();
                ps.sort_by_key(|p| versions::parse(p).num);
                if let [a, b] = ps.as_slice() {
                    let (a, b) = (a.clone(), b.clone());
                    self.details = "Comparing…".into();
                    let (tx, ctx2) = self.send();
                    std::thread::spawn(move || {
                        let text = match (als::snapshot(&a), als::snapshot(&b)) {
                            (Ok(x), Ok(y)) => {
                                let ch = changes::diff(&x, &y);
                                let mut lines = vec![format!("{}  →  {}", stem(&a), stem(&b)), String::new(), format!("Summary: {}", changes::summary_line(&ch)), String::new()];
                                lines.extend(changes::format_lines(&ch));
                                lines.join("\n")
                            }
                            (Err(e), _) | (_, Err(e)) => format!("Couldn't compare: {e}"),
                        };
                        let _ = tx.send(Msg::Compare(text));
                        ctx2.request_repaint();
                    });
                }
            }
            Some("convert") => {
                if let Some(p) = sel_path(self) {
                    self.open_convert(p, false);
                }
            }
            Some("report") => {
                if let Some(p) = sel_path(self) {
                    self.open_convert(p, true);
                }
            }
            _ => {}
        }
    }

    // ---- convert
    fn open_convert(&mut self, path: PathBuf, report_view: bool) {
        self.screen = Screen::Convert;
        self.conv = Some(ConvertUi {
            path: path.clone(),
            plan: None,
            error: None,
            sel: 0,
            busy: Some("Reading the set and your installed VST3 plug-ins…".into()),
            result: None,
            report_view,
            busy_note: None,
        });
        let (tx, ctx) = self.send();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Plan(vst3::analyze(&path, None)));
            ctx.request_repaint();
        });
    }

    fn convert_ui(&mut self, ctx: &egui::Context) {
        let mut go_back = false;
        let mut do_convert = false;
        let mut do_learn = false;
        let mut show_new: Option<PathBuf> = None;
        let Some(c) = self.conv.as_mut() else { return };
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(18.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new(format!("Convert to VST3 — {}", stem(&c.path))).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(if self.only_screen { "Version browser" } else { "← Back" }).clicked() {
                        go_back = true;
                    }
                });
            });
            ui.add_space(4.0);
            if let Some(b) = &c.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(b).color(DIM));
                });
                return;
            }
            if let Some(e) = &c.error {
                ui.label(RichText::new(e).color(BAD));
                return;
            }
            if let Some((out, report)) = &c.result {
                ui.label(RichText::new(format!("Saved as {}", out.file_name().unwrap_or_default().to_string_lossy())).color(GOOD).strong());
                ui.label(RichText::new("The original set wasn't changed. Open the new version in Live and check each converted plug-in.").color(DIM));
                ui.add_space(8.0);
                egui::Frame::none().fill(FIELD).rounding(4.0).inner_margin(12.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(ui.available_height() - 60.0).auto_shrink([false, true]).show(ui, |ui| {
                        show_text(ui, &report.join("\n"));
                    });
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add(accent_button("Open in Live")).clicked() {
                        open_in_live(out);
                    }
                    if ui.button("Show in version browser").clicked() {
                        show_new = Some(out.clone());
                    }
                });
                return;
            }
            let Some(plan) = c.plan.as_mut() else { return };
            if plan.items.is_empty() {
                ui.label("This set has no VST2 or Audio Unit plug-ins. Nothing to convert.");
                return;
            }
            let n_conv = plan.items.iter().filter(|i| i.mode != vst3::Mode::Skip).count();
            ui.label(
                RichText::new(format!(
                    "{} VST2/AU plug-ins found, {} can be replaced ({} VST3s installed). The original set isn't changed: the result is saved as the next version. \
                     Automation needs the VST3's parameter IDs, which are learned from any set that already uses that VST3.",
                    plan.items.len(),
                    n_conv,
                    plan.catalog_size
                ))
                .color(DIM),
            );
            // e.g. a Live 9 set: show why, and keep Convert disabled.
            let can_convert = n_conv > 0 && plan.blocked.is_none();
            if let Some(why) = &plan.blocked {
                ui.add_space(6.0);
                egui::Frame::none().fill(Color32::from_rgb(0x3a, 0x22, 0x22)).rounding(4.0).inner_margin(10.0).show(ui, |ui| {
                    ui.label(RichText::new(why).color(BAD));
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut c.report_view, false, RichText::new("Plug-ins").strong());
                ui.selectable_value(&mut c.report_view, true, RichText::new("Report").strong())
                    .on_hover_text("What converting will do with the choices above, without changing anything");
            });
            ui.add_space(6.0);
            if c.report_view {
                let report = vst3::preview_report(plan);
                let text = report.join("\n");
                egui::Frame::none().fill(FIELD).rounding(4.0).inner_margin(12.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(ui.available_height() - 60.0).auto_shrink([false, false]).show(ui, |ui| {
                        show_text(ui, &text);
                    });
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(can_convert, accent_button("Convert → save as new version")).clicked() {
                        do_convert = true;
                    }
                    if ui.button("Copy report").clicked() {
                        ui.ctx().copy_text(text.clone());
                        c.busy_note = Some("Report copied".into());
                    }
                    if ui.button("Save report…").clicked() {
                        let name = format!("{} - plug-in report.txt", stem(&c.path));
                        let dlg = rfd::FileDialog::new().set_title("Save plug-in report").set_file_name(&name).add_filter("Text", &["txt"]);
                        let dlg = match c.path.parent() {
                            Some(d) => dlg.set_directory(d),
                            None => dlg,
                        };
                        if let Some(dest) = dlg.save_file() {
                            let nl = if cfg!(windows) { "\r\n" } else { "\n" };
                            let body = text.replace('\n', nl) + nl;
                            c.busy_note = Some(match std::fs::write(&dest, body) {
                                Ok(()) => format!("Saved {}", dest.file_name().unwrap_or_default().to_string_lossy()),
                                Err(e) => format!("Couldn't save: {e}"),
                            });
                        }
                    }
                    if let Some(n) = &c.busy_note {
                        ui.label(RichText::new(n).color(DIM));
                    }
                });
                return;
            }
            let mut clicked = None;
            let table_h = (ui.available_height() - 110.0).max(120.0);
            egui::Frame::none().fill(FIELD).rounding(4.0).show(ui, |ui| {
                ui.set_height(table_h);
                TableBuilder::new(ui)
                    .striped(true)
                    .sense(egui::Sense::click())
                    .max_scroll_height(table_h)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(190.0).clip(true))
                    .column(Column::initial(230.0).clip(true))
                    .column(Column::initial(210.0).clip(true))
                    .column(Column::initial(280.0).clip(true))
                    .column(Column::remainder())
                    .header(28.0, |mut h| {
                        for t in ["  Track", "Current plug-in", "VST3", "Settings", "Automation / mappings"] {
                            h.col(|ui| {
                                ui.label(RichText::new(t).color(DIM).small());
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(28.0, plan.items.len(), |mut row| {
                            let i = row.index();
                            let it = &plan.items[i];
                            row.set_selected(c.sel == i);
                            let color = match it.mode {
                                vst3::Mode::Skip => DIM,
                                vst3::Mode::Vst2State => GOOD,
                                _ => WARN,
                            };
                            row.col(|ui| {
                                ui.label(format!("  {}", it.track));
                            });
                            row.col(|ui| {
                                ui.label(it.old_label());
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(it.new_label()).color(color));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(it.mode.short()).color(color));
                            });
                            row.col(|ui| {
                                let t = if it.automated.is_empty() {
                                    "—".to_string()
                                } else if it.mode == vst3::Mode::Skip {
                                    format!("{} (unchanged)", it.automated.len())
                                } else if it.mapped.len() == it.automated.len() {
                                    format!("{}/{} kept", it.mapped.len(), it.automated.len())
                                } else {
                                    format!("{}/{} kept ⚠", it.mapped.len(), it.automated.len())
                                };
                                ui.label(t);
                            });
                            if row.response().clicked() {
                                clicked = Some(i);
                            }
                        });
                    });
            });
            if let Some(i) = clicked {
                c.sel = i;
            }
            ui.add_space(8.0);
            if let Some(it) = plan.items.get_mut(c.sel) {
                ui.horizontal(|ui| {
                    ui.label(format!("{}:", it.old_label()));
                    let modes = it.modes();
                    egui::ComboBox::from_id_salt("mode").width(380.0).selected_text(it.mode.long()).show_ui(ui, |ui| {
                        for m in modes {
                            ui.selectable_value(&mut it.mode, m, m.long());
                        }
                    });
                });
                ui.label(RichText::new(&it.note).color(DIM).small());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(can_convert, accent_button("Convert → save as new version")).clicked() {
                    do_convert = true;
                }
                if ui.button("Learn parameter IDs from a folder…").on_hover_text("Pick a folder with sets that already use the VST3 versions").clicked() {
                    do_learn = true;
                }
            });
        });

        if do_convert {
            if let Some(plan) = c.plan.take() {
                c.busy = Some("Converting…".into());
                let (tx, ctx2) = (self.tx.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let r = vst3::convert(&plan, None).and_then(|(out, report)| {
                        versions::write_notes(&out, Some(&plan.path), Some(report.clone()), None)?;
                        Ok((out, report))
                    });
                    let _ = tx.send(Msg::Converted(r));
                    ctx2.request_repaint();
                });
            }
        }
        if do_learn {
            if let Some(folder) = rfd::FileDialog::new().set_title("Folder with sets that use the VST3 versions").pick_folder() {
                c.busy = Some("Learning parameter IDs…".into());
                let (tx, ctx2) = (self.tx.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let n = vst3::learn_paths(&[folder], |_, _| {});
                    let _ = tx.send(Msg::Learned(n));
                    ctx2.request_repaint();
                });
            }
        }
        if let Some(p) = show_new {
            self.screen = Screen::Browser;
            self.only_screen = false;
            self.follow_live = false;
            self.load(&p);
        } else if go_back {
            let p = self.conv.as_ref().map(|c| c.path.clone());
            self.screen = Screen::Browser;
            self.only_screen = false;
            if self.rows.is_empty() {
                if let Some(p) = p {
                    self.follow_live = false;
                    self.load(&p);
                }
            }
        }
    }

    // ---- Live Sets
    fn open_sets(&mut self) {
        // Opened on purpose: stay here (the default start sets this again after).
        self.sets_until_live = false;
        self.screen = Screen::Sets;
        if self.sets.is_none() {
            let cfg = config::load();
            let folders: Vec<PathBuf> = cfg.set_folders.iter().map(PathBuf::from).collect();
            let has_recent = !sets::live_pref_dirs().is_empty() || config::app_dir().join("recent_sets.json").exists();
            self.sets = Some(SetsUi {
                // Nothing opened yet but folders chosen: start on the folders.
                tab: if !has_recent && !folders.is_empty() { SetsTab::Folders } else { SetsTab::Recent },
                recent: None,
                scanned: sets::cached_scan(&folders),
                scanning: None,
                scan_id: 0,
                cancel: Arc::new(AtomicBool::new(false)),
                folders,
                filter: String::new(),
                per_project: cfg.sets_one_per_project,
                sel: None,
            });
            self.start_scan();
        }
        self.reload_recent();
    }

    fn reload_recent(&mut self) {
        let (tx, ctx) = self.send();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Recent(sets::recent()));
            ctx.request_repaint();
        });
    }

    fn start_scan(&mut self) {
        let (tx, ctx) = self.send();
        let Some(s) = self.sets.as_mut() else { return };
        s.cancel.store(true, Ordering::SeqCst);
        if s.folders.is_empty() {
            s.scanning = None;
            s.scanned = Some(vec![]);
            return;
        }
        s.cancel = Arc::new(AtomicBool::new(false));
        s.scan_id += 1;
        s.scanning = Some(0);
        let (id, cancel, roots) = (s.scan_id, s.cancel.clone(), s.folders.clone());
        std::thread::spawn(move || {
            let found = sets::scan(&roots, &cancel, |n| {
                let _ = tx.send(Msg::ScanProgress(id, n));
                ctx.request_repaint();
            });
            if !cancel.load(Ordering::SeqCst) {
                sets::save_scan(&roots, &found);
                let _ = tx.send(Msg::Scanned(id, found));
                ctx.request_repaint();
            }
        });
    }

    fn save_set_folders(&mut self) {
        let Some(s) = self.sets.as_ref() else { return };
        let mut c = config::load();
        c.set_folders = s.folders.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        config::save(&c);
        self.cfg.set_folders = c.set_folders;
        self.start_scan();
    }

    fn sets_ui(&mut self, ctx: &egui::Context) {
        enum Act {
            View(PathBuf),
            Open(PathBuf),
            Reveal(PathBuf),
            Forget(SetEntry),
            AddFolder,
            RemoveFolder(usize),
            Rescan,
            Browser,
            Settings,
        }
        let mut act: Option<Act> = None;
        let mut new_sel: Option<String> = None;
        let open_key = self.current_open.as_deref().map(sets::key_for);
        let Some(s) = self.sets.as_mut() else { return };

        egui::TopBottomPanel::top("sets_top").frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 18.0, right: 18.0, top: 14.0, bottom: 8.0 })).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Live Sets").strong());
                ui.add_space(16.0);
                ui.selectable_value(&mut s.tab, SetsTab::Recent, "Recently opened");
                ui.selectable_value(&mut s.tab, SetsTab::Folders, "In folders");
                ui.add_space(12.0);
                if ui
                    .checkbox(&mut s.per_project, "One per project folder")
                    .on_hover_text("Show only the most recently saved set of each project folder (a project can hold several sets, e.g. a radio edit and an extended mix)")
                    .changed()
                {
                    let mut c = config::load();
                    c.sets_one_per_project = s.per_project;
                    config::save(&c);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        act = Some(Act::Settings);
                    }
                    if ui.button("Version browser").clicked() {
                        act = Some(Act::Browser);
                    }
                    ui.add(egui::TextEdit::singleline(&mut s.filter).hint_text("Filter").desired_width(200.0));
                });
            });
            ui.label(
                RichText::new(match s.tab {
                    SetsTab::Recent => "The latest saved version of each set you've opened in Live.",
                    SetsTab::Folders => "The latest saved version of each set in these folders and their subfolders, most recently saved first.",
                })
                .color(DIM)
                .small(),
            );
            if s.tab == SetsTab::Folders {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (i, f) in s.folders.iter().enumerate() {
                        egui::Frame::none().fill(PANEL).rounding(4.0).inner_margin(egui::Margin::symmetric(8.0, 2.0)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(f.display().to_string()).small());
                                if ui.small_button("✕").on_hover_text("Stop scanning this folder").clicked() {
                                    act = Some(Act::RemoveFolder(i));
                                }
                            });
                        });
                    }
                    if ui.button("Add folder…").clicked() {
                        act = Some(Act::AddFolder);
                    }
                    if !s.folders.is_empty() && ui.add_enabled(s.scanning.is_none(), egui::Button::new("Rescan")).clicked() {
                        act = Some(Act::Rescan);
                    }
                });
            }
        });

        let list: Option<&Vec<SetEntry>> = match s.tab {
            SetsTab::Recent => s.recent.as_ref(),
            SetsTab::Folders => s.scanned.as_ref(),
        };
        let needle = s.filter.trim().to_lowercase();
        let matching = list
            .into_iter()
            .flatten()
            .filter(|e| needle.is_empty() || e.base.to_lowercase().contains(&needle) || e.folder().to_string_lossy().to_lowercase().contains(&needle));
        let shown: Vec<(&SetEntry, usize)> = if s.per_project { sets::one_per_folder(matching) } else { matching.map(|e| (e, 1)).collect() };

        egui::TopBottomPanel::bottom("sets_bottom").frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 18.0, right: 18.0, top: 8.0, bottom: 14.0 })).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let total = list.map(|l| if s.per_project { sets::one_per_folder(l).len() } else { l.len() }).unwrap_or(0);
                let what = if s.per_project { "projects" } else { "sets" };
                let mut t = if needle.is_empty() { format!("{total} {what}") } else { format!("{} of {total} {what}", shown.len()) };
                if let (SetsTab::Folders, Some(n)) = (s.tab, s.scanning) {
                    t = format!("Scanning… {n} folders looked at   ·   {t}");
                }
                ui.label(RichText::new(t).color(DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Double-click a set to see its versions").color(DIM).small());
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 18.0, right: 18.0, top: 4.0, bottom: 4.0 })).show(ctx, |ui| {
            let empty = match (s.tab, list) {
                (_, None) => Some(("Looking for your sets…", false)),
                (SetsTab::Folders, _) if s.folders.is_empty() => Some(("Add the folders where you keep your Live projects.", true)),
                (SetsTab::Folders, Some(l)) if l.is_empty() && s.scanning.is_some() => Some(("Scanning…", false)),
                (SetsTab::Folders, Some(l)) if l.is_empty() => Some(("No Live sets in these folders.", false)),
                (SetsTab::Recent, Some(l)) if l.is_empty() => Some(("Sets you open in Live will show up here.", false)),
                _ if shown.is_empty() => Some(("No sets match the filter.", false)),
                _ => None,
            };
            if let Some((msg, add)) = empty {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(msg).color(DIM));
                    if add {
                        ui.add_space(8.0);
                        if ui.add(accent_button("Add folder…")).clicked() {
                            act = Some(Act::AddFolder);
                        }
                    }
                });
                return;
            }
            let recent_tab = s.tab == SetsTab::Recent;
            egui::Frame::none().fill(FIELD).rounding(4.0).show(ui, |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(300.0).at_least(160.0).clip(true).resizable(true))
                    .column(Column::exact(130.0).clip(true))
                    .column(Column::exact(140.0))
                    .column(Column::exact(64.0))
                    .column(Column::remainder().at_least(120.0).clip(true))
                    .column(Column::exact(170.0))
                    .header(28.0, |mut h| {
                        for t in ["  Set", "Latest version", "Saved", "Versions", "Folder", ""] {
                            h.col(|ui| {
                                ui.label(RichText::new(t).color(DIM).small());
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(30.0, shown.len(), |mut row| {
                            let (e, in_folder) = shown[row.index()];
                            let key = e.key();
                            row.set_selected(s.sel.as_deref() == Some(key.as_str()));
                            let is_open = open_key.as_deref() == Some(key.as_str());
                            row.col(|ui| {
                                let t = if is_open { format!("  ▶ {}", e.base) } else { format!("  {}", e.base) };
                                ui.label(RichText::new(t).color(if is_open { ACCENT } else { FG }).strong()).on_hover_text(e.latest.display().to_string());
                                if in_folder > 1 {
                                    ui.label(RichText::new(format!("+{} more sets", in_folder - 1)).color(DIM).small())
                                        .on_hover_text("Other sets in this project folder. View shows them under “Other sets in this folder”; untick “One per project folder” to list them all.");
                                }
                            });
                            row.col(|ui| {
                                ui.label(&e.label);
                            });
                            row.col(|ui| {
                                let saved: chrono::DateTime<chrono::Local> = chrono::DateTime::from_timestamp(e.saved, 0).unwrap_or_default().into();
                                ui.label(RichText::new(saved.format("%d %b %Y  %H:%M").to_string()).color(DIM));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(e.versions.to_string()).color(DIM));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(e.folder().display().to_string()).color(DIM).small()).on_hover_text(e.folder().display().to_string());
                            });
                            row.col(|ui| {
                                if ui.small_button("View").on_hover_text("See its versions and change notes").clicked() {
                                    act = Some(Act::View(e.latest.clone()));
                                }
                                if ui.add(accent_button("Open in Live").small()).on_hover_text(e.latest.display().to_string()).clicked() {
                                    act = Some(Act::Open(e.latest.clone()));
                                }
                            });
                            let resp = row.response();
                            if resp.double_clicked() {
                                act = Some(Act::View(e.latest.clone()));
                            } else if resp.clicked() {
                                new_sel = Some(key.clone());
                            }
                            resp.context_menu(|ui| {
                                if ui.button("View versions").clicked() {
                                    act = Some(Act::View(e.latest.clone()));
                                    ui.close_menu();
                                }
                                if ui.button("Open in Live").clicked() {
                                    act = Some(Act::Open(e.latest.clone()));
                                    ui.close_menu();
                                }
                                if ui.button("Show in folder").clicked() {
                                    act = Some(Act::Reveal(e.latest.clone()));
                                    ui.close_menu();
                                }
                                if recent_tab && ui.button("Remove from this list").clicked() {
                                    act = Some(Act::Forget(e.clone()));
                                    ui.close_menu();
                                }
                            });
                        });
                    });
            });
        });

        if let (Some(k), Some(s)) = (new_sel, self.sets.as_mut()) {
            s.sel = Some(k);
        }
        match act {
            Some(Act::View(p)) => {
                self.follow_live = false;
                self.screen = Screen::Browser;
                self.load(&p);
            }
            Some(Act::Open(p)) => {
                open_in_live(&p);
                self.status = format!("Opening {} in Live…", stem(&p));
            }
            Some(Act::Reveal(p)) => reveal(&p),
            Some(Act::Forget(e)) => {
                sets::forget(&e);
                if let Some(l) = self.sets.as_mut().and_then(|s| s.recent.as_mut()) {
                    l.retain(|x| x.key() != e.key());
                }
            }
            Some(Act::AddFolder) => {
                if let Some(f) = rfd::FileDialog::new().set_title("Choose a folder with Live projects").pick_folder() {
                    if let Some(s) = self.sets.as_mut() {
                        if !s.folders.contains(&f) {
                            s.folders.push(f);
                        }
                        s.tab = SetsTab::Folders;
                    }
                    self.save_set_folders();
                }
            }
            Some(Act::RemoveFolder(i)) => {
                if let Some(s) = self.sets.as_mut() {
                    if i < s.folders.len() {
                        s.folders.remove(i);
                    }
                }
                self.save_set_folders();
            }
            Some(Act::Rescan) => self.start_scan(),
            Some(Act::Browser) => {
                self.screen = Screen::Browser;
                if self.rows.is_empty() {
                    if let Some(p) = live_set() {
                        self.load(&p);
                    }
                }
            }
            Some(Act::Settings) => self.open_settings(),
            None => {}
        }
    }

    // ---- settings
    fn open_settings(&mut self) {
        #[cfg(windows)]
        let startup = crate::win::startup_enabled();
        #[cfg(not(windows))]
        let startup = false;
        self.settings = Some(SettingsUi { cfg: config::load(), startup, message: String::new() });
        self.settings_from_sets = self.screen == Screen::Sets;
        self.screen = Screen::Settings;
    }

    fn settings_ui(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let Some(s) = self.settings.as_mut() else { return };
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(22.0)).show(ctx, |ui| {
            ui.heading(RichText::new("Settings").strong());
            ui.add_space(10.0);
            let hk_err = |spec: &str| -> Option<String> {
                #[cfg(windows)]
                {
                    crate::win::tray::parse_hotkey(spec).err()
                }
                #[cfg(not(windows))]
                {
                    let _ = spec;
                    None
                }
            };
            egui::Grid::new("g").num_columns(2).spacing([16.0, 12.0]).show(ui, |ui| {
                ui.label("Save a new version");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.cfg.hotkey_save_version).desired_width(200.0));
                    match hk_err(&s.cfg.hotkey_save_version) {
                        Some(e) => ui.label(RichText::new(e).color(BAD).small()),
                        None => ui.label(RichText::new("Works while Ableton Live is the active window").color(DIM).small()),
                    };
                });
                ui.end_row();
                ui.label("Open the version browser");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.cfg.hotkey_open_browser).desired_width(200.0));
                    match hk_err(&s.cfg.hotkey_open_browser) {
                        Some(e) => ui.label(RichText::new(e).color(BAD).small()),
                        None => ui.label(RichText::new("e.g. ctrl+alt+v, ctrl+shift+f9").color(DIM).small()),
                    };
                });
                ui.end_row();
                ui.label("New version names");
                ui.vertical(|ui| {
                    let current = VERSION_PRESETS
                        .iter()
                        .find(|(p, _)| *p == s.cfg.version_format)
                        .map(|(_, l)| l.to_string())
                        .unwrap_or_else(|| "Custom".into());
                    egui::ComboBox::from_id_salt("vfmt").selected_text(current).width(300.0).show_ui(ui, |ui| {
                        for (p, l) in VERSION_PRESETS {
                            ui.selectable_value(&mut s.cfg.version_format, p.to_string(), *l);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Pattern").color(DIM));
                        ui.add(egui::TextEdit::singleline(&mut s.cfg.version_format).desired_width(110.0).font(egui::TextStyle::Monospace));
                        match version_format_error(&s.cfg) {
                            Some(e) => ui.label(RichText::new(e).color(BAD).small()),
                            None => {
                                let f = versions::configured_format(&s.cfg);
                                ui.label(RichText::new(format!("My Song{}.als", f.render(2))).color(GOOD))
                            }
                        };
                    });
                    ui.label(RichText::new("The text between the name and the number, then one # per digit (## = 02).").color(DIM).small());
                    ui.checkbox(&mut s.cfg.keep_existing_format, "Keep the format a set already uses")
                        .on_hover_text("A set without a number takes the format of its other versions, else the one used most recently in its folder");
                });
                ui.end_row();
                ui.label("Change notes go");
                ui.vertical(|ui| {
                    ui.radio_value(&mut s.cfg.notes_location, "subfolder".to_string(), "in a subfolder of the project:");
                    ui.add_enabled(s.cfg.notes_location == "subfolder", egui::TextEdit::singleline(&mut s.cfg.notes_subfolder).desired_width(200.0));
                    ui.radio_value(&mut s.cfg.notes_location, "beside".to_string(), "next to each set (\"<set> - changes.txt\")");
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.cfg.auto_write_missing_notes, "Write notes for older versions when the browser shows them");
                ui.end_row();
                if cfg!(windows) {
                    ui.label("");
                    ui.checkbox(&mut s.startup, "Start Live Versions with Windows");
                    ui.end_row();
                }
            });
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let ok = hk_err(&s.cfg.hotkey_save_version).is_none()
                    && hk_err(&s.cfg.hotkey_open_browser).is_none()
                    && version_format_error(&s.cfg).is_none();
                if ui.add_enabled(ok, accent_button("Save")).clicked() {
                    // Live Sets options are changed in that window: keep what's on disk.
                    let disk = config::load();
                    s.cfg.set_folders = disk.set_folders;
                    s.cfg.sets_one_per_project = disk.sets_one_per_project;
                    config::save(&s.cfg);
                    #[cfg(windows)]
                    if s.startup != crate::win::startup_enabled() {
                        crate::win::set_startup(s.startup);
                    }
                    s.message = "Saved. The tray icon picks up new hotkeys within a few seconds.".into();
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
            if !s.message.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(&s.message).color(GOOD));
            }
        });
        if close {
            if self.only_screen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.cfg = config::load();
                self.screen = if self.settings_from_sets { Screen::Sets } else { Screen::Browser };
            }
        }
    }
}

/// Version name presets: (pattern, how it looks).
const VERSION_PRESETS: &[(&str, &str)] = &[
    (" v_#", "Song v_2"),
    ("_#", "Song_2  (Live Enhancement Suite)"),
    (" v#", "Song v2"),
    (" v###", "Song v002"),
    (" T##", "Song T02"),
];

/// Why a version pattern can't be used, if it can't.
fn version_format_error(cfg: &config::Config) -> Option<String> {
    let Some(f) = versions::VersionFormat::from_pattern(&cfg.version_format) else {
        return Some("End with one # per digit, e.g.  v_#  or  T##".into());
    };
    // Names made with it must be recognised as versions again, or they'd stop grouping.
    let name = format!("My Song{}", f.render(2));
    match versions::split_version(&name, &versions::words(cfg)) {
        Some((base, _, 2, _)) if base == "My Song" => None,
        _ => Some(format!("\"{name}\" wouldn't be recognised as a version")),
    }
}

fn ui_ctrl(resp: &egui::Response) -> bool {
    resp.ctx.input(|i| i.modifiers.command || i.modifiers.ctrl)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        // Dropped .als files: show their versions.
        let dropped: Option<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(p) = dropped {
            self.follow_live = false;
            self.screen = Screen::Browser;
            self.load(&p);
        }
        match self.screen {
            Screen::Browser => {
                self.poll();
                self.browser_ui(ctx);
            }
            Screen::Convert => self.convert_ui(ctx),
            Screen::Settings => self.settings_ui(ctx),
            Screen::Sets => {
                self.poll();
                self.sets_ui(ctx);
            }
        }
        let title = match self.screen {
            Screen::Sets => "Live Versions — Live Sets",
            Screen::Settings => "Live Versions — Settings",
            Screen::Convert => "Live Versions — Convert to VST3",
            Screen::Browser => "Live Versions",
        };
        if self.title != title {
            self.title = title.to_string();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title.clone()));
        }
        ctx.request_repaint_after(Duration::from_millis(1000));
    }
}
