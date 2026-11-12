//! The notification bell: an accent dot while there's anything unread, and a
//! struck-through bell while Do Not Disturb is on.

use gtk::prelude::*;
use gtk4 as gtk;

use crate::notifications;
use crate::theme::{self, FAINT, SUBTLE};

pub fn bell(s: f64) -> gtk::Overlay {
    let px = 14.0 * s;
    let glyph = theme::filled_icon_glyph("notifications", px, SUBTLE);
    let size = (px + 4.0 * s).round() as i32;
    glyph.set_size_request(size, size);
    let dot = gtk::DrawingArea::builder()
        .content_width((6.0 * s).round() as i32)
        .content_height((6.0 * s).round() as i32)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top((1.0 * s).round() as i32)
        .margin_end((1.0 * s).round() as i32)
        .can_target(false)
        .build();
    dot.set_draw_func(|_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
        theme::set_source(cr, theme::palette().accent_lit);
        let _ = cr.fill();
    });
    let bell = gtk::Overlay::new();
    bell.set_child(Some(&glyph));
    bell.add_overlay(&dot);

    let paint = {
        let (glyph, dot) = (glyph.downgrade(), dot.downgrade());
        move || {
            let (Some(glyph), Some(dot)) = (glyph.upgrade(), dot.upgrade()) else { return };
            let quiet = notifications::dnd();
            let name = if quiet { "notifications_off" } else { "notifications" };
            glyph.set_text(name);
            glyph.set_attributes(theme::filled_icon_glyph(name, px, if quiet { FAINT } else { SUBTLE }).attributes().as_ref());
            dot.set_visible(notifications::unread() > 0);
            dot.queue_draw();
        }
    };
    paint();
    notifications::on_change(move |_| paint());
    bell
}
