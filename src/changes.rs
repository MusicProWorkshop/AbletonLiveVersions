//! Compare two snapshots and describe the changes that matter.
//!
//! Deliberately ignored: clip contents, automation curves, small mixer moves,
//! track colours and device parameter tweaks. Those change on nearly every save
//! and would bury the interesting parts (tracks, instruments, devices, structure).
use crate::als::{Snapshot, TrackSnap};
use std::collections::HashMap;

#[derive(Default, Debug, Clone)]
pub struct Changes {
    pub set: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub renamed: Vec<String>,
    pub instruments: Vec<String>,
    pub devices: Vec<String>,
    pub groups: Vec<String>,
    pub samples: Vec<String>,
    pub clips: Vec<String>,
    pub state: Vec<String>,
}

fn q(s: &str) -> String {
    format!("\"{s}\"")
}

/// Number formatting like Python's `:g` (72.0 -> "72", 126.5 -> "126.5").
pub fn g(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn inst(t: &TrackSnap) -> String {
    t.instruments.join(", ")
}

fn plural(n: i64, word: &str) -> String {
    if n.abs() == 1 { word.to_string() } else { format!("{word}s") }
}

/// Items in `a` not matched by an equal item in `b` (multiset difference, keeps order).
fn multiset_minus(a: &[String], b: &[String]) -> Vec<String> {
    let mut counts: HashMap<&str, i32> = HashMap::new();
    for x in b {
        *counts.entry(x.as_str()).or_default() += 1;
    }
    let mut out = Vec::new();
    for x in a {
        let c = counts.entry(x.as_str()).or_default();
        if *c > 0 { *c -= 1 } else { out.push(x.clone()) }
    }
    out
}

/// Pair tracks by Live's persistent track Id, then by type+name for the rest.
fn match_tracks<'a>(old: &'a [TrackSnap], new: &'a [TrackSnap]) -> (Vec<(&'a TrackSnap, &'a TrackSnap)>, Vec<&'a TrackSnap>, Vec<&'a TrackSnap>) {
    let mut used = vec![false; old.len()];
    let mut pairs = Vec::new();
    let mut unmatched = Vec::new();
    for t in new {
        match old.iter().position(|o| o.id == t.id && o.ttype == t.ttype) {
            Some(i) if !used[i] => {
                used[i] = true;
                pairs.push((&old[i], t));
            }
            _ => unmatched.push(t),
        }
    }
    let mut added = Vec::new();
    for t in unmatched {
        match old.iter().enumerate().position(|(i, o)| !used[i] && o.ttype == t.ttype && o.ident == t.ident) {
            Some(i) => {
                used[i] = true;
                pairs.push((&old[i], t));
            }
            None => added.push(t),
        }
    }
    let removed = old.iter().enumerate().filter(|(i, _)| !used[*i]).map(|(_, o)| o).collect();
    (pairs, added, removed)
}

pub fn diff(old: &Snapshot, new: &Snapshot) -> Changes {
    let mut ch = Changes::default();
    if old.tempo != new.tempo {
        if let (Some(a), Some(b)) = (old.tempo, new.tempo) {
            ch.set.push(format!("Tempo {} → {} BPM", g(a), g(b)));
        }
    }
    // Only when Scale mode is on in both: toggling it on/off is not a key change.
    if !old.key.is_empty() && !new.key.is_empty() && old.key != new.key {
        ch.set.push(format!("Key {} → {}", old.key, new.key));
    }
    if old.scenes != new.scenes {
        ch.set.push(format!("Scenes {} → {}", old.scenes, new.scenes));
    }
    let (ob, nb) = (old.length_beats / 4.0, new.length_beats / 4.0);
    if (nb - ob).abs() >= 1.0 {
        ch.set.push(format!("Arrangement length {} → {} bars", g(ob), g(nb)));
    }
    let la = multiset_minus(&new.locators, &old.locators);
    let lr = multiset_minus(&old.locators, &new.locators);
    if !la.is_empty() || !lr.is_empty() {
        let parts: Vec<String> = la.iter().map(|x| format!("+ {}", q(x))).chain(lr.iter().map(|x| format!("− {}", q(x)))).take(8).collect();
        ch.set.push(format!("Locators: {}", parts.join(", ")));
    }
    if old.creator != new.creator {
        ch.set.push(format!("Saved with {} (was {})", new.creator, old.creator));
    }

    let (mut pairs, added, removed) = match_tracks(&old.tracks, &new.tracks);
    for t in &added {
        let wher = if t.group_name.is_empty() { String::new() } else { format!(" in group {}", q(&t.group_name)) };
        let i = if t.instruments.is_empty() { String::new() } else { format!(" — {}", inst(t)) };
        let fx = if !t.effects.is_empty() && t.ttype != "MIDI" {
            format!(" (+ {})", t.effects.iter().take(4).cloned().collect::<Vec<_>>().join(", "))
        } else {
            String::new()
        };
        ch.added.push(format!("+ {} {}{wher}{i}{fx}", t.ttype, q(&t.name)));
    }
    for t in &removed {
        let i = if t.instruments.is_empty() { String::new() } else { format!(" — {}", inst(t)) };
        ch.removed.push(format!("− {} {}{i}", t.ttype, q(&t.name)));
    }
    if let (Some(a), Some(b)) = (&old.main, &new.main) {
        pairs.push((a, b));
    }

    let mut mix = 0;
    for (o, n) in pairs {
        let name = q(&n.name);
        if o.ident != n.ident {
            ch.renamed.push(format!("{} → {name}", q(&o.name)));
        }
        if o.instruments != n.instruments {
            let a = if o.instruments.is_empty() { "none".into() } else { inst(o) };
            let b = if n.instruments.is_empty() { "none".into() } else { inst(n) };
            ch.instruments.push(format!("{name}: {a} → {b}"));
        }
        let plus = multiset_minus(&n.effects, &o.effects);
        let minus = multiset_minus(&o.effects, &n.effects);
        if !plus.is_empty() || !minus.is_empty() {
            let bits: Vec<String> = plus.iter().map(|d| format!("+ {d}")).chain(minus.iter().map(|d| format!("− {d}"))).collect();
            ch.devices.push(format!("{name}: {}", bits.join(", ")));
        }
        if o.group != n.group {
            if !n.group_name.is_empty() {
                ch.groups.push(format!("{name} moved into group {}", q(&n.group_name)));
            } else {
                ch.groups.push(format!("{name} moved out of group {}", q(&o.group_name)));
            }
        }
        if o.muted != n.muted {
            ch.state.push(format!("{name} {}", if n.muted { "muted" } else { "unmuted" }));
        }
        if o.frozen != n.frozen {
            ch.state.push(format!("{name} {}", if n.frozen { "frozen" } else { "unfrozen" }));
        }
        let mut bits = Vec::new();
        let da = n.arr_clips as i64 - o.arr_clips as i64;
        let ds = n.session_clips as i64 - o.session_clips as i64;
        let dn = n.notes as i64 - o.notes as i64;
        if da != 0 {
            bits.push(format!("{da:+} arrangement {}", plural(da, "clip")));
        }
        if ds != 0 {
            bits.push(format!("{ds:+} session {}", plural(ds, "clip")));
        }
        if dn.abs() >= 4 {
            bits.push(format!("{dn:+} notes"));
        }
        if !bits.is_empty() {
            ch.clips.push(format!("{name}: {}", bits.join(", ")));
        }
        if (o.volume - n.volume).abs() > 0.02 || (o.pan - n.pan).abs() > 0.02 {
            mix += 1;
        }
        let new_samples: Vec<&String> = n.samples.iter().filter(|s| !o.samples.contains(s)).collect();
        if !new_samples.is_empty() {
            let mut shown = new_samples.iter().take(4).map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            if new_samples.len() > 4 {
                shown += &format!(" (+{} more)", new_samples.len() - 4);
            }
            ch.samples.push(format!("{name}: {shown}"));
        }
    }
    if mix > 0 {
        ch.state.push(format!("Volume/pan changed on {mix} {}", plural(mix, "track")));
    }
    ch
}

pub fn format_lines(ch: &Changes) -> Vec<String> {
    let sections: [(&Vec<String>, &str, bool); 10] = [
        (&ch.set, "Set", false),
        (&ch.added, "Tracks added", true),
        (&ch.removed, "Tracks removed", true),
        (&ch.renamed, "Tracks renamed", true),
        (&ch.instruments, "Instruments changed", true),
        (&ch.devices, "Devices added / removed", true),
        (&ch.groups, "Grouping", false),
        (&ch.samples, "New samples", false),
        (&ch.clips, "Clips & notes", false),
        (&ch.state, "Mix & track state", false),
    ];
    let mut lines = Vec::new();
    for (items, title, show_all) in sections {
        if items.is_empty() {
            continue;
        }
        lines.push(title.to_string());
        let n = if show_all { items.len() } else { items.len().min(12) };
        lines.extend(items.iter().take(n).map(|x| format!("  {x}")));
        if items.len() > n {
            lines.push(format!("  … and {} more", items.len() - n));
        }
        lines.push(String::new());
    }
    if lines.is_empty() {
        lines.push("No structural changes (only edits inside clips, automation or device settings).".into());
    }
    lines
}

pub fn summary_line(ch: &Changes) -> String {
    let mut bits: Vec<String> = Vec::new();
    if !ch.added.is_empty() {
        let names: Vec<&str> = ch.added.iter().filter_map(|a| a.split('"').nth(1)).collect();
        if names.len() <= 2 {
            bits.push(format!("+ {}", names.join(", ")));
        } else {
            bits.push(format!("+{} tracks", names.len()));
        }
    }
    if !ch.removed.is_empty() {
        bits.push(format!("−{} {}", ch.removed.len(), plural(ch.removed.len() as i64, "track")));
    }
    if !ch.renamed.is_empty() {
        bits.push(format!("{} renamed", ch.renamed.len()));
    }
    if !ch.instruments.is_empty() {
        let names: Vec<&str> = ch.instruments.iter().take(2).map(|x| x.split(':').next().unwrap_or("").trim_matches('"')).collect();
        bits.push(format!("new instrument on {}{}", names.join(", "), if ch.instruments.len() > 2 { "…" } else { "" }));
    }
    if !ch.devices.is_empty() {
        bits.push(format!("devices on {} {}", ch.devices.len(), plural(ch.devices.len() as i64, "track")));
    }
    for s in &ch.set {
        if s.starts_with("Tempo") || s.starts_with("Arrangement") || s.starts_with("Key") {
            bits.push(s.replace("Arrangement length", "length"));
        }
    }
    if !ch.samples.is_empty() && bits.len() < 4 {
        bits.push("new samples".into());
    }
    if bits.is_empty() && !ch.clips.is_empty() {
        bits.push(format!("clip edits on {} {}", ch.clips.len(), plural(ch.clips.len() as i64, "track")));
    }
    if bits.is_empty() && !ch.state.is_empty() {
        bits.push("mix tweaks".into());
    }
    if bits.is_empty() { "minor edits".into() } else { bits.join(" · ") }
}

pub fn counts_line(s: &Snapshot) -> String {
    let parts: Vec<String> = ["MIDI", "Audio", "Group", "Return"]
        .iter()
        .filter_map(|k| {
            let n = s.tracks.iter().filter(|t| t.ttype == *k).count();
            (n > 0).then(|| format!("{n} {k}"))
        })
        .collect();
    format!("{} tracks ({})", s.tracks.len(), parts.join(", "))
}

pub fn set_overview(s: &Snapshot) -> String {
    let mut bits = vec![s.creator.clone()];
    if let Some(t) = s.tempo {
        bits.push(format!("{} BPM", g(t)));
    }
    if !s.key.is_empty() {
        bits.push(s.key.clone());
    }
    bits.push(counts_line(s));
    if s.length_beats > 0.0 {
        bits.push(format!("{} bars", g(s.length_beats / 4.0)));
    }
    bits.retain(|b| !b.is_empty());
    bits.join(" · ")
}

pub fn contents_lines(s: &Snapshot) -> Vec<String> {
    s.tracks
        .iter()
        .map(|t| {
            let indent = if t.group_name.is_empty() { "  " } else { "    " };
            let i = if t.instruments.is_empty() { String::new() } else { format!(" — {}", inst(t)) };
            format!("{indent}{} {}{i}", t.ttype, q(&t.name))
        })
        .collect()
}
