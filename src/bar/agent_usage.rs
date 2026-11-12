//! Claude Code rate-limit readout: a sparkle and the five-hour window's
//! usage, tinted with the accent past 80%.

use gtk::pango;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::agent_usage;
use crate::theme::{self, TextStyle, SUBTLE};

const WARN_ABOVE: u32 = 80;

pub fn agent_usage(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (3.0 * s).round() as i32);
    let glyph = theme::filled_icon_glyph("auto_awesome", 15.0 * s, SUBTLE);
    let value = gtk::Label::new(None);
    row.append(&glyph);
    row.append(&value);

    let paint = move || {
        let percent = agent_usage::current().session.map(|l| l.percent);
        let color = if percent.is_some_and(|p| p > WARN_ABOVE) { theme::palette().accent_lit } else { SUBTLE };
        glyph.set_attributes(theme::filled_icon_glyph("auto_awesome", 15.0 * s, color).attributes().as_ref());
        value.set_text(&percent.map_or("\u{2013}".into(), |p| format!("{p}%")));
        TextStyle::new(9.5 * s, pango::Weight::Medium, color).apply(&value);
    };
    paint();
    theme::on_palette_change(paint.clone());
    agent_usage::on_change(paint);
    row
}
