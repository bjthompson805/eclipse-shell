//! Apps: desktop entries ranked by how well the name matches, then by
//! frecency (launches, worth less as they age), so what you use rises.
//! Launch counts live in ~/.local/state/eclipse-shell/launches.json.

use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;

use crate::launcher::results::{Entry, Icon};

/// Launches lose half their weight every this many days.
const HALF_LIFE_DAYS: f64 = 7.0;
pub const MAX_RESULTS: usize = 40;

fn launches_file() -> std::path::PathBuf {
    glib::user_state_dir().join("eclipse-shell").join("launches.json")
}

/// App id -> (launch count, last launch as Unix seconds).
fn launches() -> HashMap<String, (f64, i64)> {
    std::fs::read_to_string(launches_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

/// How much `id` has been used lately.
fn frecency(table: &HashMap<String, (f64, i64)>, id: &str) -> f64 {
    let Some(&(count, last)) = table.get(id) else { return 0.0 };
    let days = (now() - last).max(0) as f64 / 86_400.0;
    count * 0.5f64.powf(days / HALF_LIFE_DAYS)
}

fn record(id: &str) {
    let mut table = launches();
    // Decay the old count to now before adding, so the count stays a weight.
    let weight = frecency(&table, id);
    table.insert(id.to_owned(), (weight + 1.0, now()));
    let file = launches_file();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, serde_json::to_string(&table).unwrap_or_default());
}

fn id(app: &gio::AppInfo) -> String {
    app.id().map(String::from).unwrap_or_else(|| app.display_name().to_string())
}

pub fn launch(app: &gio::AppInfo) {
    record(&id(app));
    let context = gdk::Display::default().map(|d| d.app_launch_context());
    if let Err(err) = app.launch(&[], context.as_ref()) {
        eprintln!("eclipse-shell: launching {} failed: {err}", app.display_name());
    }
}

/// Apps matching `query`, best first.
pub fn search(apps: &[gio::AppInfo], query: &str) -> Vec<Entry> {
    let query = query.trim().to_lowercase();
    let table = launches();
    let mut scored: Vec<(u32, f64, Entry)> = apps
        .iter()
        .filter_map(|app| {
            let title = app.display_name().to_string();
            let (tier, matched) = score(app, &title, &query)?;
            Some((tier, frecency(&table, &id(app)), entry(app, title, matched)))
        })
        .collect();
    // Stable, so equal scores stay alphabetical.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
    scored.into_iter().take(MAX_RESULTS).map(|(_, _, e)| e).collect()
}

/// Every app, most used first, then by name; for the all-apps grid.
pub fn all_by_use(apps: &[gio::AppInfo]) -> Vec<gio::AppInfo> {
    let table = launches();
    let mut out = apps.to_vec();
    out.sort_by(|a, b| frecency(&table, &id(b)).total_cmp(&frecency(&table, &id(a))));
    out
}

fn entry(app: &gio::AppInfo, title: String, matched: Vec<(usize, usize)>) -> Entry {
    let launched = app.clone();
    Entry {
        icon: app.icon().map_or(Icon::Glyph("apps"), Icon::Gicon),
        subtitle: subtitle(app),
        title,
        matched,
        verb: "Launch",
        action: Rc::new(move || launch(&launched)),
    }
}

/// Higher is better. Name prefix, then a word prefix, then anywhere in the
/// name, then the command, then the query's letters in order.
fn score(app: &gio::AppInfo, title: &str, query: &str) -> Option<(u32, Vec<(usize, usize)>)> {
    // (byte offset, lowercased char) for each char of the title.
    let chars: Vec<(usize, char)> = title.char_indices().map(|(i, c)| (i, c.to_lowercase().next().unwrap_or(c))).collect();
    let lowered: String = chars.iter().map(|&(_, c)| c).collect();
    let span = |char_start: usize, len: usize| {
        let start = chars[char_start].0;
        let end = chars.get(char_start + len).map_or(title.len(), |&(i, _)| i);
        vec![(start, end)]
    };
    let qlen = query.chars().count();
    // Byte offset in `lowered` of each char, to slice it by char index.
    let offsets: Vec<usize> = lowered.char_indices().map(|(b, _)| b).collect();

    if lowered.starts_with(query) {
        return Some((4, span(0, qlen)));
    }
    let mut word_start = true;
    for (i, &(_, c)) in chars.iter().enumerate() {
        if word_start && lowered[offsets[i]..].starts_with(query) {
            return Some((3, span(i, qlen)));
        }
        word_start = !c.is_alphanumeric();
    }
    if let Some(byte) = lowered.find(query) {
        let at = offsets.binary_search(&byte).unwrap_or(0);
        return Some((2, span(at, qlen)));
    }
    // The program's own name (not its flags: "--password-store=basic"
    // shouldn't make "ba" find a browser).
    if program(app).is_some_and(|p| p.starts_with(query) || (qlen >= 3 && p.contains(query))) {
        return Some((1, Vec::new()));
    }
    // Every query letter, in order, highlighted where it landed; only for
    // longer queries, where it's a useful guess rather than noise.
    if qlen < 3 {
        return None;
    }
    let mut matched = Vec::new();
    let mut rest = chars.iter().enumerate();
    for q in query.chars() {
        let (i, _) = rest.find(|(_, (_, c))| *c == q)?;
        matched.extend(span(i, 1));
    }
    Some((0, matched))
}

/// The program an app runs, lowercased: the first word of its command
/// that isn't `env` or a VAR=value, without its folder.
fn program(app: &gio::AppInfo) -> Option<String> {
    let line = app.commandline()?.to_string_lossy().into_owned();
    let word = line.split_whitespace().find(|w| *w != "env" && !w.contains('='))?;
    Some(word.rsplit('/').next().unwrap_or(word).to_lowercase())
}

fn subtitle(app: &gio::AppInfo) -> String {
    let Some(desktop) = app.downcast_ref::<gio_unix::DesktopAppInfo>() else { return String::new() };
    if let Some(generic) = desktop.generic_name().filter(|g| !g.is_empty()) {
        return generic.to_string();
    }
    let categories = desktop.categories().map(|c| c.to_string()).unwrap_or_default();
    if categories.is_empty() {
        return String::new();
    }
    const KINDS: &[(&str, &str)] = &[
        ("TerminalEmulator", "Terminal"),
        ("WebBrowser", "Browser"),
        ("InstantMessaging", "Chat"),
        ("Audio", "Media"),
        ("AudioVideo", "Media"),
        ("Video", "Media"),
        ("Game", "Game"),
        ("Development", "Dev"),
        ("Graphics", "Graphics"),
        ("Office", "Office"),
        ("Settings", "System"),
        ("System", "System"),
        ("Utility", "Tool"),
        ("Network", "Net"),
    ];
    let listed: Vec<&str> = categories.split(';').collect();
    KINDS.iter().find(|(key, _)| listed.contains(key)).map_or("App", |(_, kind)| kind).to_owned()
}
