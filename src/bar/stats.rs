//! CPU, memory and temperature readout: a glyph and a percentage each, tinted
//! with the accent once a value runs hot.
//!
//! Values come from the shared system-stats poller.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::pango;
use gtk4 as gtk;

use crate::sysstats;
use crate::theme::{self, TextStyle, SUBTLE};

struct Item {
    row: gtk::Box,
    glyph: gtk::Label,
    value: gtk::Label,
    name: &'static str,
    warn_above: u32,
}

impl Item {
    fn new(s: f64, name: &'static str, warn_above: u32) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, (3.0 * s).round() as i32);
        let glyph = theme::icon_glyph(name, 14.0 * s, SUBTLE);
        let value = gtk::Label::new(None);
        row.append(&glyph);
        row.append(&value);
        let item = Self { row, glyph, value, name, warn_above };
        item.show(s, None, "");
        item
    }

    fn show(&self, s: f64, value: Option<u32>, unit: &str) {
        self.row.set_visible(value.is_some());
        let Some(value) = value else { return };
        let color = if value > self.warn_above { theme::palette().accent_lit } else { SUBTLE };
        let glyph = theme::filled_icon_glyph(self.name, 14.0 * s, color);
        self.glyph.set_attributes(glyph.attributes().as_ref());
        self.value.set_text(&format!("{value}{unit}"));
        TextStyle::new(9.5 * s, pango::Weight::Medium, color).apply(&self.value);
    }
}

pub fn stats(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (11.0 * s).round() as i32);
    let cpu = Item::new(s, "memory", 85);
    let mem = Item::new(s, "developer_board", 90);
    let temp = Item::new(s, "device_thermostat", 80);
    for item in [&cpu, &mem, &temp] {
        row.append(&item.row);
    }

    let paint = Rc::new(move || {
        let stats = sysstats::current();
        cpu.show(s, stats.cpu, "%");
        mem.show(s, stats.mem, "%");
        temp.show(s, stats.temp, "\u{b0}");
    });
    theme::on_palette_change({
        let paint = paint.clone();
        move || paint()
    });
    sysstats::on_change(move || paint());
    row
}
