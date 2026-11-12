//! The help panel (F1, or the "?" button): what each prefix does and the
//! keys, in two columns.

use gtk::pango;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::theme::{self, TextStyle, CREAM, DIM, FAINT};

const MODES: &[(&str, &str, &str)] = &[
    ("", "apps", "Apps first (most used rise), then open windows and a web search"),
    ("/", "actions", "Shell panels, lock, wallpaper, screenshots, media; ←→ switch tabs"),
    ("/file", "files", "Also /folder, /image and /video, then part of the name"),
    ("=", "calculator", "2^10, sqrt(2), 5!, sin(pi/2); Enter copies the answer"),
    ("?", "web", "DuckDuckGo with an instant answer; ?!yt, ?!w, ?!gh… pick a site"),
    (">", "packages", "Search pkg; Enter installs or removes in a terminal"),
    ("@", "radio", "Live stations; Enter plays, again to stop"),
];

const KEYS: &[(&str, &str)] = &[
    ("↑ ↓  Tab", "move the selection"),
    ("Enter", "run the selected row"),
    ("Ctrl+A", "all apps"),
    ("F1", "this help"),
    ("Esc", "close"),
];

pub fn panel(s: f64) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let column = gtk::Box::new(gtk::Orientation::Vertical, px(9.0));
    column.set_margin_start(px(18.0));
    column.set_margin_end(px(18.0));
    column.set_margin_top(px(12.0));
    column.set_margin_bottom(px(14.0));
    column.append(&crate::popout::micro_label(s, "Type a prefix"));
    let accent = theme::palette().accent_lit;
    for (prefix, name, what) in MODES {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, px(12.0));
        let key = TextStyle::new(12.0 * s, pango::Weight::Bold, accent).mono().label(if prefix.is_empty() { "abc" } else { prefix });
        key.set_width_chars(5);
        key.set_xalign(0.0);
        row.append(&key);
        let text = gtk::Box::new(gtk::Orientation::Vertical, px(1.0));
        let title = TextStyle::new(11.5 * s, pango::Weight::Semibold, CREAM).label(name);
        title.set_xalign(0.0);
        text.append(&title);
        let detail = TextStyle::new(10.0 * s, pango::Weight::Normal, DIM).label(what);
        detail.set_xalign(0.0);
        detail.set_wrap(true);
        detail.set_max_width_chars(48);
        text.append(&detail);
        row.append(&text);
        column.append(&row);
    }
    column.append(&crate::popout::divider());
    let keys = gtk::Grid::builder().column_spacing(px(18.0)).row_spacing(px(4.0)).build();
    for (i, (key, what)) in KEYS.iter().enumerate() {
        let k = TextStyle::new(10.0 * s, pango::Weight::Semibold, CREAM).mono().label(key);
        k.set_xalign(0.0);
        let w = TextStyle::new(10.0 * s, pango::Weight::Normal, FAINT).label(what);
        w.set_xalign(0.0);
        let (col, row) = ((i % 2) as i32 * 2, (i / 2) as i32);
        keys.attach(&k, col, row, 1, 1);
        keys.attach(&w, col + 1, row, 1, 1);
    }
    column.append(&keys);
    column
}
