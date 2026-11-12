//! Open windows: matched by title and class, focused on Enter. Listed under
//! the apps, so "fire" shows Firefox and then its open window.

use std::rc::Rc;

use crate::hyprland;
use crate::icons;
use crate::launcher::results::{Entry, Icon};

#[derive(Clone)]
pub struct Window {
    address: String,
    class: String,
    title: String,
    workspace: String,
}

/// The windows open now, read once as the launcher opens.
pub fn read() -> Vec<Window> {
    let Ok(text) = hyprland::request("j/clients") else { return Vec::new() };
    let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else { return Vec::new() };
    list.iter()
        .filter(|c| c["mapped"].as_bool() != Some(false) && c["workspace"]["id"].as_i64().is_some_and(|id| id > 0))
        .filter_map(|c| {
            Some(Window {
                address: c["address"].as_str()?.to_owned(),
                class: c["class"].as_str().unwrap_or_default().to_owned(),
                title: c["title"].as_str().unwrap_or_default().to_owned(),
                workspace: c["workspace"]["name"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

pub fn search(windows: &[Window], query: &str) -> Vec<Entry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    windows
        .iter()
        .filter(|w| w.title.to_lowercase().contains(&query) || w.class.to_lowercase().contains(&query))
        .map(|w| {
            let address = w.address.clone();
            let title = if w.title.is_empty() { w.class.clone() } else { w.title.clone() };
            // Lowercasing can shift byte offsets; only highlight when it didn't.
            let lowered = title.to_lowercase();
            let matched = lowered.find(&query).filter(|_| lowered.len() == title.len()).map(|at| vec![(at, at + query.len())]).unwrap_or_default();
            Entry {
                icon: Icon::Gicon(icons::for_class(&w.class)),
                matched,
                title,
                subtitle: format!("Window · workspace {}", w.workspace),
                verb: "Switch",
                action: Rc::new(move || hyprland::focus_window(&address)),
            }
        })
        .collect()
}
