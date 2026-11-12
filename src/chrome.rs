//! Window chrome for the shell's own apps (the wallpaper browser, and the
//! hub to come). They're undecorated, so they draw their own close button.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::{popout, theme};

/// An X that closes `window`, lit on hover; for a header's right end.
pub fn close_button(s: f64, window: &gtk::ApplicationWindow) -> gtk::DrawingArea {
    let size = (26.0 * s).round() as i32;
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).build();
    let hovered = Rc::new(Cell::new(false));
    area.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            if hovered.get() {
                cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, 2.0 * std::f64::consts::PI);
                theme::set_source(cr, palette.accent.alpha(0.22));
                let _ = cr.fill();
            }
            let arm = w.min(h) * 0.2;
            cr.set_line_width(1.6 * s);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            theme::set_source(cr, if hovered.get() { palette.accent_lit } else { theme::ICON_DIM });
            cr.move_to(w / 2.0 - arm, h / 2.0 - arm);
            cr.line_to(w / 2.0 + arm, h / 2.0 + arm);
            cr.move_to(w / 2.0 + arm, h / 2.0 - arm);
            cr.line_to(w / 2.0 - arm, h / 2.0 + arm);
            let _ = cr.stroke();
        }
    });
    popout::on_hover(&area, {
        let area = area.downgrade();
        move |inside| {
            hovered.set(inside);
            area.upgrade().into_iter().for_each(|a| a.queue_draw());
        }
    });
    let window = window.downgrade();
    popout::on_click(&area, move || window.upgrade().into_iter().for_each(|w| w.close()));
    area
}
