//! File search, reached only by name so a plain search never wades through
//! the disk: "/file", "/folder", "/image" and "/video" followed by words.
//! A bounded walk of the usual places (hidden folders and build trees
//! skipped), on a worker thread; Enter opens the file with its default app.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;

use super::Cache;
use crate::launcher::results::{Entry, Icon};

const MAX: usize = 40;
const MAX_DEPTH: usize = 8;
const BUDGET: Duration = Duration::from_millis(1500);
const SKIP: &[&str] = &["node_modules", "target", "__pycache__", "build", "dist", "venv"];
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "svg", "heic", "avif"];
const VIDEOS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    Image,
    Video,
}

impl Kind {
    /// "/file query" -> (File, "query"), for each of the four words.
    pub fn split(text: &str) -> Option<(Kind, &str)> {
        for (word, kind) in [("file", Kind::File), ("folder", Kind::Folder), ("image", Kind::Image), ("video", Kind::Video)] {
            if let Some(rest) = text.strip_prefix(word) {
                if rest.is_empty() || rest.starts_with(' ') {
                    return Some((kind, rest.trim()));
                }
            }
        }
        None
    }

    fn glyph(self) -> &'static str {
        match self {
            Kind::File => "description",
            Kind::Folder => "folder",
            Kind::Image => "image",
            Kind::Video => "movie",
        }
    }
}

thread_local! {
    static CACHE: Rc<Cache<Vec<PathBuf>>> = Cache::new();
}

pub fn search(kind: Kind, query: &str, notify: &Rc<dyn Fn()>) -> (Vec<Entry>, &'static str) {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return (Vec::new(), "Type part of the name");
    }
    let key = format!("{}:{}", kind.glyph(), words.join(" "));
    let found = CACHE.with(|c| c.get(&key, notify, move || walk(kind, &words)));
    match found {
        None => (Vec::new(), "Looking…"),
        Some(paths) => (paths.into_iter().map(|p| entry(kind, p)).collect(), "Nothing by that name"),
    }
}

fn roots(kind: Kind) -> Vec<PathBuf> {
    let home = glib::home_dir();
    match kind {
        Kind::File | Kind::Folder => vec![home],
        Kind::Image => vec![home.join("Pictures"), home.join("Downloads"), home.join("Desktop")],
        Kind::Video => vec![home.join("Videos"), home.join("Downloads"), home.join("Desktop")],
    }
}

/// Runs on a worker thread: breadth first, so shallow matches come first.
fn walk(kind: Kind, words: &[String]) -> Vec<PathBuf> {
    let started = Instant::now();
    let mut found: Vec<PathBuf> = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> = roots(kind).into_iter().map(|r| (r, 0)).collect();
    while let Some((dir, depth)) = queue.pop_front() {
        if found.len() >= MAX * 4 || started.elapsed() > BUDGET {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind_of) = e.file_type() else { continue };
            let path = e.path();
            if kind_of.is_dir() {
                if depth < MAX_DEPTH && !SKIP.contains(&name.as_str()) {
                    queue.push_back((path.clone(), depth + 1));
                }
                if kind != Kind::Folder {
                    continue;
                }
            } else if kind == Kind::Folder {
                continue;
            }
            let extension = path.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
            if (kind == Kind::Image && !IMAGES.contains(&extension.as_str())) || (kind == Kind::Video && !VIDEOS.contains(&extension.as_str())) {
                continue;
            }
            let lowered = name.to_lowercase();
            if words.iter().all(|w| lowered.contains(w.as_str())) {
                found.push(path);
            }
        }
    }
    // Names that start with the first word lead.
    found.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        (!name.starts_with(words[0].as_str()), p.components().count())
    });
    found.truncate(MAX);
    found
}

fn entry(kind: Kind, path: PathBuf) -> Entry {
    let home = glib::home_dir();
    let place = path.parent().map(|p| p.strip_prefix(&home).map_or_else(|_| p.display().to_string(), |rest| format!("~/{}", rest.display()))).unwrap_or_default();
    let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Entry {
        icon: Icon::Glyph(kind.glyph()),
        title,
        subtitle: place,
        matched: Vec::new(),
        verb: "Open",
        action: Rc::new(move || open(&path)),
    }
}

fn open(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    let context = gtk::gdk::Display::default().map(|d| d.app_launch_context());
    if let Err(err) = gio::AppInfo::launch_default_for_uri(&uri, context.as_ref()) {
        eprintln!("eclipse-shell: opening {} failed: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_the_command() {
        assert!(matches!(Kind::split("file notes"), Some((Kind::File, "notes"))));
        assert!(matches!(Kind::split("folder"), Some((Kind::Folder, ""))));
        assert!(Kind::split("files").is_none());
        assert!(Kind::split("lock").is_none());
    }
}
