//! Network throughput: download over upload, from the shared traffic poller
//! (src/traffic.rs). The value column is as wide as the widest reading
//! ("1023.9 KB/s"), so moving between B/s, KB/s and MB/s never shifts the
//! modules beside it.

use gtk::prelude::*;
use gtk::pango;
use gtk4 as gtk;

use crate::theme::{self, TextStyle, SUBTLE};
use crate::traffic;

const WIDEST: &str = "1023.9 KB/s";

pub fn net_speed(s: f64) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, (1.0 * s).round() as i32);
    let style = TextStyle::new(8.5 * s, pango::Weight::Medium, SUBTLE);
    let row = |glyph: &str| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, (3.0 * s).round() as i32);
        row.append(&theme::filled_icon_glyph(glyph, 10.0 * s, SUBTLE));
        let value = style.label("\u{2013}");
        value.set_xalign(0.0);
        value.set_width_chars(WIDEST.chars().count() as i32);
        row.append(&value);
        column.append(&row);
        value
    };
    let down = row("arrow_downward");
    let up = row("arrow_upward");

    let (down, up) = (down.downgrade(), up.downgrade());
    let paint = move || {
        let (Some(down), Some(up)) = (down.upgrade(), up.upgrade()) else { return };
        let (d, u) = match traffic::now() {
            Some(rate) => (traffic::format_bytes(rate.down, true), traffic::format_bytes(rate.up, true)),
            None => ("\u{2013}".to_owned(), "\u{2013}".to_owned()),
        };
        down.set_text(&d);
        up.set_text(&u);
    };
    paint();
    traffic::on_change(paint);
    column
}
