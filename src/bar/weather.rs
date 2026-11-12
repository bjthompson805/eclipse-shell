//! Weather: the condition glyph and the temperature, from the shared weather
//! feed. Hidden until the first reading arrives.

use gtk::pango;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::theme::{self, TextStyle, SUBTLE};
use crate::weather::{self, Glyph};

pub fn weather(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (4.0 * s).round() as i32);
    let glyph = theme::filled_icon_glyph("cloud", 14.0 * s, SUBTLE);
    glyph.set_size_request((16.0 * s).round() as i32, (16.0 * s).round() as i32);
    let temp = TextStyle::new(10.5 * s, pango::Weight::Medium, SUBTLE).label("");
    row.append(&glyph);
    row.append(&temp);

    let paint = {
        let row = row.downgrade();
        move || {
            let Some(row) = row.upgrade() else { return };
            let Some(now) = weather::current() else { return row.set_visible(false) };
            let name = match now.glyph {
                Glyph::Sun => "clear_day",
                Glyph::Cloud => "cloud",
                Glyph::Fog => "foggy",
                Glyph::Rain => "rainy",
                Glyph::Snow => "weather_snowy",
                Glyph::Storm => "thunderstorm",
            };
            glyph.set_text(name);
            temp.set_text(&now.temp);
            row.set_visible(true);
        }
    };
    paint();
    weather::on_change(paint);
    row
}
