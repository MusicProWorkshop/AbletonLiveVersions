//! The windows: version browser, VST3 converter and settings (egui).
use eframe::egui::{self, Color32, RichText, Vec2};
use egui_extras::{Column, TableBuilder};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        Start::Settings => ("Live Versions — Settings", [660.0, 470.0]),
        Start::Convert(_) | Start::Report(_) => ("Live Versions — Convert to VST3", [1120.0, 640.0]),
        Start::Browser(_) => ("Live Versions", [1200.0, 720.0]),
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
}

#[derive(PartialEq)]
enum Screen {
    Browser,
    Convert,
    Settings,
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
        };
        match start {
            Start::Browser(p) => {
                let p = p.or_else(live_set);
                app.last_live = live_set();
                if let Some(p) = p {
                    app.load(&p);
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
            if let (true, Some(p)) = (self.follow_live, &live) {
                let same_family = self.folder.as_deref() == p.parent() && versions::parse(p).base.trim() == self.base;
                if !same_family && self.screen == Screen::Browser {
                    self.load(p);
                    return;
                }
            }
            self.current_open = live;
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
                    .column(Column::exact(96.0))
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
                    if ui.add_enabled(n_conv > 0, accent_button("Convert → save as new version")).clicked() {
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
                if ui.add_enabled(n_conv > 0, accent_button("Convert → save as new version")).clicked() {
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

    // ---- settings
    fn open_settings(&mut self) {
        #[cfg(windows)]
        let startup = crate::win::startup_enabled();
        #[cfg(not(windows))]
        let startup = false;
        self.settings = Some(SettingsUi { cfg: config::load(), startup, message: String::new() });
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
                let ok = hk_err(&s.cfg.hotkey_save_version).is_none() && hk_err(&s.cfg.hotkey_open_browser).is_none();
                if ui.add_enabled(ok, accent_button("Save")).clicked() {
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
                self.screen = Screen::Browser;
            }
        }
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
        }
        ctx.request_repaint_after(Duration::from_millis(1000));
    }
}
