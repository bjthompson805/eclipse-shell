//! Web search: "?" searches the web (DuckDuckGo unless a leading !bang picks
//! a site), and shows DuckDuckGo's instant answer above it once it arrives.
//! A plain query that matches nothing else offers a web search too.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;

use crate::launcher::results::{Entry, Icon};

/// (bang, site, search URL prefix)
const ENGINES: &[(&str, &str, &str)] = &[
    ("ddg", "DuckDuckGo", "https://duckduckgo.com/?q="),
    ("g", "Google", "https://www.google.com/search?q="),
    ("yt", "YouTube", "https://www.youtube.com/results?search_query="),
    ("w", "Wikipedia", "https://en.wikipedia.org/w/index.php?search="),
    ("gh", "GitHub", "https://github.com/search?q="),
    ("r", "Reddit", "https://www.reddit.com/search/?q="),
    ("fp", "FreshPorts", "https://www.freshports.org/search.php?query="),
    ("man", "FreeBSD man pages", "https://man.freebsd.org/cgi/man.cgi?query="),
    ("crates", "crates.io", "https://crates.io/search?q="),
    ("maps", "OpenStreetMap", "https://www.openstreetmap.org/search?query="),
];

#[derive(Clone)]
struct Answer {
    text: String,
    source: String,
    url: String,
}

thread_local! {
    /// Instant answers by query; None while asked or when there's none.
    static ANSWERS: RefCell<HashMap<String, Option<Answer>>> = RefCell::default();
    static PENDING: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

fn open(url: &str) {
    let context = gtk::gdk::Display::default().map(|d| d.app_launch_context());
    if let Err(err) = gio::AppInfo::launch_default_for_uri(url, context.as_ref()) {
        eprintln!("eclipse-shell: opening {url} failed: {err}");
    }
}

fn encode(text: &str) -> String {
    glib::Uri::escape_string(text, None, false).to_string()
}

/// Splits a leading "!bang" off; an unknown bang searches the whole text.
fn route(text: &str) -> (&'static (&'static str, &'static str, &'static str), String) {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('!') {
        let (bang, query) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if let Some(engine) = ENGINES.iter().find(|(b, _, _)| b.eq_ignore_ascii_case(bang)) {
            return (engine, query.trim().to_owned());
        }
    }
    (&ENGINES[0], text.to_owned())
}

pub fn search_entry(text: &str) -> Option<Entry> {
    let ((_, site, prefix), query) = route(text);
    if query.is_empty() {
        return None;
    }
    let url = format!("{prefix}{}", encode(&query));
    Some(Entry {
        icon: Icon::Glyph("travel_explore"),
        title: format!("Search {site} for \u{201c}{query}\u{201d}"),
        subtitle: url.clone(),
        matched: Vec::new(),
        verb: "Open",
        action: Rc::new(move || open(&url)),
    })
}

/// The "?" mode: the answer (once it's in), the search, and the bangs.
pub fn search(text: &str, notify: &Rc<dyn Fn()>) -> Vec<Entry> {
    let mut out = Vec::new();
    let text = text.trim();
    if text.is_empty() {
        for (bang, site, _) in ENGINES {
            out.push(Entry {
                icon: Icon::Glyph("travel_explore"),
                title: format!("!{bang}"),
                subtitle: format!("Search {site}: ?!{bang} your words"),
                matched: Vec::new(),
                verb: "",
                action: Rc::new(|| {}),
            });
        }
        return out;
    }
    let bang = text.starts_with('!');
    if !bang {
        if let Some(answer) = answer(text, notify) {
            let url = answer.url.clone();
            out.push(Entry {
                icon: Icon::Glyph("lightbulb"),
                title: answer.text,
                subtitle: format!("{} · DuckDuckGo instant answer", answer.source),
                matched: Vec::new(),
                verb: "Open",
                action: Rc::new(move || open(&url)),
            });
        }
    }
    out.extend(search_entry(text));
    out
}

/// The cached answer for `query`; asks for it (a moment after typing stops)
/// and calls `notify` when it arrives.
fn answer(query: &str, notify: &Rc<dyn Fn()>) -> Option<Answer> {
    if let Some(known) = ANSWERS.with(|a| a.borrow().get(query).cloned()) {
        return known;
    }
    PENDING.with(|p| {
        if let Some(old) = p.borrow_mut().take() {
            old.remove();
        }
    });
    let (query, notify) = (query.to_owned(), notify.clone());
    let id = glib::timeout_add_local_once(Duration::from_millis(350), move || {
        PENDING.with(|p| p.borrow_mut().take());
        ANSWERS.with(|a| a.borrow_mut().insert(query.clone(), None));
        glib::spawn_future_local(async move {
            let q = query.clone();
            let found = gio::spawn_blocking(move || fetch(&q)).await.ok().flatten();
            if found.is_some() {
                ANSWERS.with(|a| a.borrow_mut().insert(query, found));
                notify();
            }
        });
    });
    PENDING.with(|p| *p.borrow_mut() = Some(id));
    None
}

/// Runs on a worker thread.
fn fetch(query: &str) -> Option<Answer> {
    let url = format!("https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1", encode(query));
    let out = std::process::Command::new("curl").args(["-fsSL", "--max-time", "6", &url]).output().ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let text = |k: &str| json[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    if let Some(answer) = text("Answer") {
        return Some(Answer { text: answer, source: text("AnswerType").unwrap_or_else(|| "Answer".into()), url: format!("https://duckduckgo.com/?q={}", encode(query)) });
    }
    let abstract_text = text("AbstractText")?;
    // One sentence or so fits a row.
    let short = abstract_text.split(". ").next().unwrap_or(&abstract_text).to_owned();
    Some(Answer { text: short, source: text("AbstractSource").unwrap_or_else(|| "Wikipedia".into()), url: text("AbstractURL").unwrap_or_else(|| format!("https://duckduckgo.com/?q={}", encode(query))) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_bangs() {
        assert_eq!(route("!yt cats").0 .1, "YouTube");
        assert_eq!(route("!yt cats").1, "cats");
        assert_eq!(route("plain words").0 .1, "DuckDuckGo");
        assert_eq!(route("!nope cats").1, "!nope cats");
    }
}
