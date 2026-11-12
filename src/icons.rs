//! App icons: window class → desktop entry → themed icon.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::config;

thread_local! {
    static CACHE: RefCell<HashMap<String, gio::Icon>> = RefCell::default();
}

/// Uses the same icon theme as the Qt side of the desktop, so app icons
/// match everywhere.
pub fn use_desktop_theme() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_icon_theme_name(Some(config::get().icon_theme.as_str()));
    }
}

/// The icon for windows of `class`, falling back to an icon named after the
/// class itself (which many apps install) and then a generic one.
pub fn for_class(class: &str) -> gio::Icon {
    if let Some(icon) = CACHE.with(|c| c.borrow().get(class).cloned()) {
        return icon;
    }
    let icon = from_desktop_entry(class).unwrap_or_else(|| {
        gio::ThemedIcon::from_names(&[&class.to_lowercase(), "application-x-executable"]).upcast()
    });
    CACHE.with(|c| c.borrow_mut().insert(class.to_owned(), icon.clone()));
    icon
}

fn from_desktop_entry(class: &str) -> Option<gio::Icon> {
    let lower = class.to_lowercase();
    for id in [format!("{class}.desktop"), format!("{lower}.desktop")] {
        if let Some(icon) = gio_unix::DesktopAppInfo::new(&id).and_then(|app| app.icon()) {
            return Some(icon);
        }
    }
    // Otherwise the entry that claims this class, or whose id or command
    // matches it (e.g. class "kitty" → kitty.desktop's Exec=kitty).
    gio::AppInfo::all().into_iter().find_map(|app| {
        let desktop = app.downcast_ref::<gio_unix::DesktopAppInfo>()?;
        let claims = desktop.startup_wm_class().is_some_and(|wm| wm.to_lowercase() == lower);
        let id_matches = app.id().is_some_and(|id| id.trim_end_matches(".desktop").to_lowercase() == lower);
        // Not executable(): gio-rs maps it to a PathBuf, but it's NULL for
        // some entries and converting that segfaults.
        let exe = app.commandline().and_then(|c| c.to_string_lossy().split_whitespace().next().map(String::from));
        let exe_matches = exe.is_some_and(|exe| exe.rsplit('/').next().unwrap_or(&exe).to_lowercase() == lower);
        (claims || id_matches || exe_matches).then(|| app.icon()).flatten()
    })
}
