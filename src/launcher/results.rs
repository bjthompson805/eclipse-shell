//! Ranked app results: a section header over up to eight visible rows. The
//! keyboard selection and the pointer hover are tracked separately, so moving
//! the mouse never changes what Enter launches.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, BRIGHT, CHIP_EDGE, CHIP_FILL, CREAM, DIM, FAINT, HAIR, ROW_HOVER};

pub const MAX_RESULTS: usize = 40;
pub const VISIBLE_ROWS: usize = 8;

pub struct Entry {
    pub app: gio::AppInfo,
    title: String,
    subtitle: String,
    /// Byte ranges of `title` that matched the query.
    matched: Vec<(usize, usize)>,
}

/// Ranks `apps` (already in alphabetical order) against `query`.
pub fn search(apps: &[gio::AppInfo], query: &str) -> Vec<Entry> {
    let query = query.trim().to_lowercase();
    let mut scored: Vec<(u32, Entry)> = apps
        .iter()
        .filter_map(|app| {
            let title = app.display_name().to_string();
            let (score, matched) = score(app, &title, &query)?;
            Some((score, Entry { app: app.clone(), subtitle: subtitle(app), title, matched }))
        })
        .collect();
    // Stable, so equal scores stay alphabetical.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().take(MAX_RESULTS).map(|(_, e)| e).collect()
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
    if app.executable().to_string_lossy().to_lowercase().contains(query) {
        return Some((1, Vec::new()));
    }
    // Every query letter, in order, highlighted where it landed.
    let mut matched = Vec::new();
    let mut rest = chars.iter().enumerate();
    for q in query.chars() {
        let (i, _) = rest.find(|(_, (_, c))| *c == q)?;
        matched.extend(span(i, 1));
    }
    Some((0, matched))
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

struct Row {
    widget: gtk::Overlay,
    background: gtk::DrawingArea,
    icon: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    verb: gtk::Label,
}

pub struct ResultList {
    pub widget: gtk::Box,
    s: f64,
    entries: RefCell<Vec<Entry>>,
    rows: Vec<Row>,
    /// Index of the first visible entry.
    offset: Cell<usize>,
    selected: Cell<usize>,
    hovered: Cell<Option<usize>>,
    launch: RefCell<Option<Box<dyn Fn(gio::AppInfo)>>>,
}

impl ResultList {
    pub fn new(s: f64) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, (2.0 * s).round() as i32);
        widget.append(&section_header(s, "APP"));
        let rows: Vec<Row> = (0..VISIBLE_ROWS).map(|_| Row::new(s)).collect();
        for row in &rows {
            widget.append(&row.widget);
        }
        let list = Rc::new(Self {
            widget,
            s,
            entries: RefCell::default(),
            rows,
            offset: Cell::new(0),
            selected: Cell::new(0),
            hovered: Cell::new(None),
            launch: RefCell::default(),
        });
        for (slot, row) in list.rows.iter().enumerate() {
            list.connect_row(slot, row);
        }
        list
    }

    pub fn connect_launch(&self, f: impl Fn(gio::AppInfo) + 'static) {
        *self.launch.borrow_mut() = Some(Box::new(f));
    }

    /// Height for `count` results: rows, gaps and the section header.
    pub fn height(s: f64, count: usize) -> f64 {
        let rows = count.min(VISIBLE_ROWS) as f64;
        rows * 42.0 * s + 18.0 * s
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub fn set_entries(&self, entries: Vec<Entry>) {
        *self.entries.borrow_mut() = entries;
        self.offset.set(0);
        self.selected.set(0);
        self.render();
    }

    /// Moves the selection, scrolling just enough to keep it in view.
    pub fn step(&self, delta: isize) {
        let count = self.len();
        if count == 0 {
            return;
        }
        let selected = (self.selected.get() as isize + delta).clamp(0, count as isize - 1) as usize;
        self.selected.set(selected);
        let offset = self.offset.get();
        if selected < offset {
            self.offset.set(selected);
        } else if selected >= offset + VISIBLE_ROWS {
            self.offset.set(selected + 1 - VISIBLE_ROWS);
        }
        self.render();
    }

    pub fn activate_selected(&self) {
        self.activate(self.selected.get());
    }

    fn activate(&self, index: usize) {
        let Some(app) = self.entries.borrow().get(index).map(|e| e.app.clone()) else { return };
        if let Some(launch) = self.launch.borrow().as_ref() {
            launch(app);
        }
    }

    fn connect_row(self: &Rc<Self>, slot: usize, row: &Row) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let list = Rc::downgrade(self);
            move |_, _, _| {
                if let Some(list) = list.upgrade() {
                    list.hovered.set(Some(list.offset.get() + slot));
                    list.render();
                }
            }
        });
        motion.connect_leave({
            let list = Rc::downgrade(self);
            move |_| {
                if let Some(list) = list.upgrade() {
                    list.hovered.set(None);
                    list.render();
                }
            }
        });
        row.widget.add_controller(motion);

        let click = gtk::GestureClick::new();
        click.connect_released({
            let list = Rc::downgrade(self);
            move |_, _, _, _| {
                if let Some(list) = list.upgrade() {
                    list.activate(list.offset.get() + slot);
                }
            }
        });
        row.widget.add_controller(click);

        row.background.set_draw_func({
            let list = Rc::downgrade(self);
            move |_, cr, w, h| {
                let Some(list) = list.upgrade() else { return };
                let index = list.offset.get() + slot;
                let selected = index == list.selected.get();
                if !selected && list.hovered.get() != Some(index) {
                    return;
                }
                let (w, h) = (f64::from(w), f64::from(h));
                theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 9.0 * list.s);
                theme::set_source(cr, if selected { CHIP_FILL } else { ROW_HOVER });
                let _ = cr.fill_preserve();
                if selected {
                    cr.set_line_width(1.0);
                    theme::set_source(cr, CHIP_EDGE);
                    let _ = cr.stroke();
                }
                cr.new_path();
            }
        });
    }

    fn render(&self) {
        let entries = self.entries.borrow();
        let s = self.s;
        for (slot, row) in self.rows.iter().enumerate() {
            let index = self.offset.get() + slot;
            let Some(entry) = entries.get(index) else {
                row.widget.set_visible(false);
                continue;
            };
            let selected = index == self.selected.get();
            row.widget.set_visible(true);
            match entry.app.icon() {
                Some(icon) => row.icon.set_from_gicon(&icon),
                None => row.icon.set_icon_name(Some("application-x-executable")),
            }

            let title_style = if selected { TextStyle::new(13.0 * s, pango::Weight::Semibold, BRIGHT) } else { TextStyle::new(13.0 * s, pango::Weight::Normal, CREAM) };
            let attrs = title_style.attrs();
            let (r, g, b) = theme::palette().accent_lit.to_u16();
            for &(start, end) in &entry.matched {
                let mut colour = pango::AttrColor::new_foreground(r, g, b);
                colour.set_start_index(start as u32);
                colour.set_end_index(end as u32);
                attrs.insert(colour);
            }
            row.title.set_text(&entry.title);
            row.title.set_attributes(Some(&attrs));

            row.subtitle.set_text(&entry.subtitle);
            row.subtitle.set_visible(!entry.subtitle.is_empty());
            TextStyle::new(10.0 * s, pango::Weight::Normal, if selected { DIM } else { FAINT }).apply(&row.subtitle);
            row.verb.set_visible(selected);
            row.background.queue_draw();
        }
    }
}

impl Row {
    fn new(s: f64) -> Self {
        let pad = (10.0 * s).round() as i32;
        let background = gtk::DrawingArea::new();
        background.set_size_request(-1, (40.0 * s).round() as i32);

        let icon = gtk::Image::builder().pixel_size((22.0 * s).round() as i32).valign(gtk::Align::Center).build();
        let text_label = |style: TextStyle| {
            let label = style.label("");
            label.set_xalign(0.0);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label
        };
        let title = text_label(TextStyle::new(13.0 * s, pango::Weight::Normal, CREAM));
        let subtitle = text_label(TextStyle::new(10.0 * s, pango::Weight::Normal, FAINT));
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing((1.0 * s).round() as i32)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .margin_start((11.0 * s).round() as i32)
            .margin_end(pad)
            .build();
        text.append(&title);
        text.append(&subtitle);
        let verb = TextStyle::new(11.0 * s, pango::Weight::Normal, theme::palette().accent_lit).label("Launch");
        verb.set_valign(gtk::Align::Center);

        let content = gtk::Box::builder().margin_start(pad).margin_end(pad).build();
        content.append(&icon);
        content.append(&text);
        content.append(&verb);

        let widget = gtk::Overlay::new();
        widget.set_child(Some(&background));
        widget.add_overlay(&content);
        widget.set_cursor_from_name(Some("pointer"));
        Self { widget, background, icon, title, subtitle, verb }
    }
}

fn section_header(s: f64, name: &str) -> gtk::Overlay {
    let pad = 10.0 * s;
    let rule = gtk::DrawingArea::new();
    rule.set_size_request(-1, (18.0 * s).round() as i32);
    rule.set_draw_func(move |_, cr, w, h| {
        let y = (f64::from(h) / 2.0).floor();
        cr.rectangle(pad + 70.0 * s, y, f64::from(w) - pad * 2.0 - 70.0 * s, 1.0);
        theme::set_source(cr, HAIR);
        let _ = cr.fill();
    });
    let label = TextStyle::new(9.0 * s, pango::Weight::Normal, FAINT).tracking(1.0).label(name);
    label.set_halign(gtk::Align::Start);
    label.set_valign(gtk::Align::Center);
    label.set_margin_start(pad.round() as i32);
    let header = gtk::Overlay::new();
    header.set_child(Some(&rule));
    header.add_overlay(&label);
    header
}
