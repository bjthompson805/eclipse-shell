//! What the launcher finds for a query. A leading character picks the mode:
//!
//!   (none)  apps, then open windows, an answer if it reads as maths, and a
//!           web search as the last row
//!   /       actions (tabs while bare); "/file", "/folder", "/image" and
//!           "/video" search files
//!   =       calculator
//!   ?       web search, with !bangs and DuckDuckGo's instant answer
//!   >       packages
//!   @       radio
//!
//! Slow providers answer from a cache and call `notify` when more arrives.

pub mod actions;
pub mod apps;
pub mod calc;
pub mod files;
pub mod packages;
pub mod radio;
pub mod web;
pub mod windows;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::{gio, glib};
use gtk4 as gtk;

use super::results::Entry;

pub struct Context {
    pub apps: Vec<gio::AppInfo>,
    pub windows: Vec<windows::Window>,
    /// The chosen tab of a bare "/".
    pub tab: usize,
    /// Asks for the results again (more arrived).
    pub notify: Rc<dyn Fn()>,
}

pub struct Routed {
    pub section: &'static str,
    pub entries: Vec<Entry>,
    /// Tabs to show over the results, and the chosen one.
    pub tabs: Option<&'static [&'static str]>,
    /// What to say when there's nothing.
    pub empty: &'static str,
}

pub fn route(query: &str, context: &Context) -> Routed {
    let mut chars = query.chars();
    match chars.next() {
        Some('/') => {
            let rest = chars.as_str();
            if let Some((kind, words)) = files::Kind::split(rest) {
                let (entries, empty) = files::search(kind, words, &context.notify);
                return Routed { section: "FILES", entries, tabs: None, empty };
            }
            Routed { section: "ACTIONS", entries: actions::search(rest, context.tab), tabs: rest.trim().is_empty().then_some(actions::TABS), empty: "No such action" }
        }
        Some('>') => {
            let (entries, empty) = packages::search(chars.as_str(), &context.notify);
            Routed { section: "PACKAGES", entries, tabs: None, empty }
        }
        Some('@') => Routed { section: "RADIO", entries: radio::search(chars.as_str()), tabs: None, empty: "No such station" },
        Some('=') => Routed { section: "CALCULATOR", entries: calc::entry(chars.as_str()).into_iter().collect(), tabs: None, empty: "Type a sum, like 2^10 or sqrt(2)" },
        Some('?') => Routed { section: "WEB", entries: web::search(chars.as_str(), &context.notify), tabs: None, empty: "Type what to search for" },
        _ => {
            let mut entries = Vec::new();
            if calc::looks_like_math(query) {
                entries.extend(calc::entry(query));
            }
            entries.extend(apps::search(&context.apps, query));
            entries.extend(windows::search(&context.windows, query));
            entries.extend(web::search_entry(query));
            Routed { section: "RESULTS", entries, tabs: None, empty: "No matches" }
        }
    }
}

/// Answers for slow lookups, by query: asked a moment after typing stops,
/// worked out on a worker thread, and `notify` called when they arrive.
pub struct Cache<V> {
    answers: RefCell<HashMap<String, Option<V>>>,
    pending: RefCell<Option<glib::SourceId>>,
}

impl<V: Clone + Send + 'static> Cache<V> {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { answers: RefCell::default(), pending: RefCell::default() })
    }

    /// The answer for `key` if it's in; else None, and it's asked for.
    pub fn get(self: &Rc<Self>, key: &str, notify: &Rc<dyn Fn()>, work: impl FnOnce() -> V + Send + 'static) -> Option<V> {
        if let Some(known) = self.answers.borrow().get(key) {
            return known.clone();
        }
        if let Some(old) = self.pending.borrow_mut().take() {
            old.remove();
        }
        let (cache, key, notify) = (Rc::downgrade(self), key.to_owned(), notify.clone());
        let id = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(c) = cache.upgrade() else { return };
            c.pending.borrow_mut().take();
            c.answers.borrow_mut().insert(key.clone(), None);
            let cache = Rc::downgrade(&c);
            glib::spawn_future_local(async move {
                let found = gio::spawn_blocking(work).await.ok();
                if let (Some(c), Some(found)) = (cache.upgrade(), found) {
                    c.answers.borrow_mut().insert(key, Some(found));
                    notify();
                }
            });
        });
        *self.pending.borrow_mut() = Some(id);
        None
    }

    pub fn clear(&self) {
        self.answers.borrow_mut().clear();
    }
}
