//! The result rows under the search row: a section header over up to eight
//! visible rows of whatever the providers found (apps, windows, actions,
//! answers...). The keyboard selection and the pointer hover are tracked
//! separately, so moving the mouse never changes what Enter runs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, BRIGHT, CHIP_EDGE, CHIP_FILL, CREAM, DIM, FAINT, HAIR, ICON_DIM, ROW_HOVER};

pub const VISIBLE_ROWS: usize = 8;

pub enum Icon {
    /// An app's (or a window's) icon.
    Gicon(gio::Icon),
    /// A Material Symbols glyph.
    Glyph(&'static str),
}

/// One result row and what Enter (or a click) on it does.
pub struct Entry {
    pub icon: Icon,
    pub title: String,
    pub subtitle: String,
    /// Byte ranges of `title` that matched the query.
    pub matched: Vec<(usize, usize)>,
    /// The word shown on the selected row: "Launch", "Copy", "Open"...
    pub verb: &'static str,
    pub action: Rc<dyn Fn()>,
}

struct Row {
    widget: gtk::Overlay,
    background: gtk::DrawingArea,
    icon: gtk::Image,
    glyph: gtk::Label,
    title: gtk::Label,
    subtitle: gtk::Label,
    verb: gtk::Label,
}

pub struct ResultList {
    pub widget: gtk::Box,
    header: gtk::Label,
    s: f64,
    entries: RefCell<Vec<Entry>>,
    rows: Vec<Row>,
    /// Index of the first visible entry.
    offset: Cell<usize>,
    selected: Cell<usize>,
    hovered: Cell<Option<usize>>,
    run: RefCell<Option<Box<dyn Fn(Rc<dyn Fn()>)>>>,
}

impl ResultList {
    pub fn new(s: f64) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, (2.0 * s).round() as i32);
        let (header_widget, header) = section_header(s);
        widget.append(&header_widget);
        let rows: Vec<Row> = (0..VISIBLE_ROWS).map(|_| Row::new(s)).collect();
        for row in &rows {
            widget.append(&row.widget);
        }
        let list = Rc::new(Self {
            widget,
            header,
            s,
            entries: RefCell::default(),
            rows,
            offset: Cell::new(0),
            selected: Cell::new(0),
            hovered: Cell::new(None),
            run: RefCell::default(),
        });
        for (slot, row) in list.rows.iter().enumerate() {
            list.connect_row(slot, row);
        }
        list
    }

    /// `f` gets the action of a row that's activated, to run it.
    pub fn connect_run(&self, f: impl Fn(Rc<dyn Fn()>) + 'static) {
        *self.run.borrow_mut() = Some(Box::new(f));
    }

    /// The section header's text, e.g. "APPS" or "ACTIONS".
    pub fn set_section(&self, name: &str) {
        self.header.set_text(name);
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
        let Some(action) = self.entries.borrow().get(index).map(|e| e.action.clone()) else { return };
        if let Some(run) = self.run.borrow().as_ref() {
            run(action);
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
            match &entry.icon {
                Icon::Gicon(icon) => {
                    row.icon.set_from_gicon(icon);
                    row.icon.set_visible(true);
                    row.glyph.set_visible(false);
                }
                Icon::Glyph(name) => {
                    row.glyph.set_text(name);
                    row.icon.set_visible(false);
                    row.glyph.set_visible(true);
                }
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
            row.verb.set_text(entry.verb);
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
        let glyph = theme::icon_glyph("bolt", 19.0 * s, ICON_DIM);
        glyph.set_size_request((22.0 * s).round() as i32, -1);
        glyph.set_visible(false);
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
        content.append(&glyph);
        content.append(&text);
        content.append(&verb);

        let widget = gtk::Overlay::new();
        widget.set_child(Some(&background));
        widget.add_overlay(&content);
        widget.set_cursor_from_name(Some("pointer"));
        Self { widget, background, icon, glyph, title, subtitle, verb }
    }
}

fn section_header(s: f64) -> (gtk::Box, gtk::Label) {
    let pad = (10.0 * s).round() as i32;
    let row = gtk::Box::builder().spacing(pad).margin_start(pad).margin_end(pad).height_request((18.0 * s).round() as i32).build();
    let label = TextStyle::new(9.0 * s, pango::Weight::Normal, FAINT).tracking(1.0).label("APPS");
    label.set_valign(gtk::Align::Center);
    row.append(&label);
    let rule = gtk::DrawingArea::builder().hexpand(true).build();
    rule.set_draw_func(|_, cr, w, h| {
        cr.rectangle(0.0, (f64::from(h) / 2.0).floor(), f64::from(w), 1.0);
        theme::set_source(cr, HAIR);
        let _ = cr.fill();
    });
    row.append(&rule);
    (row, label)
}
