//! The desktop mark: a disc with a small companion disc tucked against its
//! upper right, separated by a cut-out gap. Drawn in code in the accent
//! colour, so it follows the wallpaper like the rest of the shell.

use std::f64::consts::PI;

use gtk::prelude::*;
use gtk::cairo;
use gtk4 as gtk;

use crate::theme;

/// The mark at `size` logical px square.
pub fn mark(size: f64) -> gtk::DrawingArea {
    let px = size.round() as i32;
    let area = gtk::DrawingArea::builder().content_width(px).content_height(px).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    area.set_draw_func(|_, cr, w, h| draw(cr, f64::from(w.min(h))));
    theme::on_palette_change({
        let area = area.downgrade();
        move || {
            if let Some(area) = area.upgrade() {
                area.queue_draw();
            }
        }
    });
    area
}

fn draw(cr: &cairo::Context, size: f64) {
    let palette = theme::palette();
    let body_r = size * 0.40;
    let (body_x, body_y) = (size * 0.44, size * 0.56);
    let moon_r = size * 0.16;
    // Centred on the body's rim, up and to the right.
    let angle = -PI / 4.0;
    let (moon_x, moon_y) = (body_x + body_r * angle.cos(), body_y + body_r * angle.sin());
    let gap = size * 0.07;

    cr.push_group();
    cr.arc(body_x, body_y, body_r, 0.0, 2.0 * PI);
    theme::set_source(cr, palette.accent);
    let _ = cr.fill();
    // Carve a gap around the moon so it reads on any background.
    cr.set_operator(cairo::Operator::Clear);
    cr.arc(moon_x, moon_y, moon_r + gap, 0.0, 2.0 * PI);
    let _ = cr.fill();
    cr.set_operator(cairo::Operator::Over);
    cr.arc(moon_x, moon_y, moon_r, 0.0, 2.0 * PI);
    theme::set_source(cr, palette.accent_lit);
    let _ = cr.fill();
    let _ = cr.pop_group_to_source();
    let _ = cr.paint();
}
