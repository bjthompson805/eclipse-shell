//! The desktop mark: a disc with a small companion disc tucked against its
//! upper right, separated by a cut-out gap. Drawn in code in the accent
//! colour, so it follows the wallpaper like the rest of the shell. The `mark`
//! setting swaps it, everywhere, for a glyph or an accent-tinted image.

use std::cell::RefCell;
use std::f64::consts::PI;

use gtk::gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use gtk::{cairo, glib};
use gtk4 as gtk;

use crate::{config, theme};

/// What the `mark` setting asks for.
enum Logo {
    Eclipse,
    Image(std::path::PathBuf),
    Text(String),
}

fn logo() -> Logo {
    let spec = config::get().mark.trim();
    if spec.is_empty() {
        return Logo::Eclipse;
    }
    match spec.strip_prefix("~/") {
        Some(rest) => Logo::Image(glib::home_dir().join(rest)),
        None if spec.starts_with('/') => Logo::Image(spec.into()),
        None => Logo::Text(spec.to_owned()),
    }
}

/// The image rasterised at `px` device px, cached since the mark redraws on
/// every palette change; `None` (once warned) if it doesn't load.
fn image(path: &std::path::Path, px: i32) -> Option<Pixbuf> {
    thread_local! {
        static CACHE: RefCell<Vec<(i32, Option<Pixbuf>)>> = const { RefCell::new(Vec::new()) };
    }
    CACHE.with_borrow_mut(|cache| {
        if let Some((_, pixbuf)) = cache.iter().find(|(size, _)| *size == px) {
            return pixbuf.clone();
        }
        let pixbuf = Pixbuf::from_file_at_scale(path, px, px, true)
            .map_err(|err| eprintln!("eclipse-shell: mark {}: {err}", path.display()))
            .ok();
        cache.push((px, pixbuf.clone()));
        pixbuf
    })
}

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

/// Draws the mark in the square (0, 0, size, size).
pub fn draw(cr: &cairo::Context, size: f64) {
    match logo() {
        Logo::Eclipse => draw_eclipse(cr, size),
        Logo::Image(path) => draw_image(cr, &path, size),
        Logo::Text(text) => draw_text(cr, &text, size),
    }
}

/// The image as a stencil filled with the accent, fitted and centred.
fn draw_image(cr: &cairo::Context, path: &std::path::Path, size: f64) {
    // Rasterised at device resolution so SVGs stay crisp on HiDPI.
    let (dev, _) = cr.user_to_device_distance(size, 0.0).unwrap_or((size, 0.0));
    let Some(pixbuf) = image(path, (dev.abs().max(size) * 2.0).ceil() as i32) else { return };
    let (pw, ph) = (f64::from(pixbuf.width()), f64::from(pixbuf.height()));
    let k = (size / pw).min(size / ph);
    let _ = cr.save();
    cr.translate((size - pw * k) / 2.0, (size - ph * k) / 2.0);
    cr.scale(k, k);
    cr.set_source_pixbuf(&pixbuf, 0.0, 0.0);
    let mask = cr.source();
    theme::set_source(cr, theme::palette().accent);
    let _ = cr.mask(&mask);
    let _ = cr.restore();
}

/// The text in the accent, its ink centred in the square.
fn draw_text(cr: &cairo::Context, text: &str, size: f64) {
    let style = theme::TextStyle::new(size,gtk::pango::Weight::Normal, theme::palette().accent);
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text(text);
    layout.set_attributes(Some(&style.attrs()));
    let (ink, _) = layout.extents();
    let s = f64::from(gtk::pango::SCALE);
    let (x, y, w, h) = (f64::from(ink.x()) / s, f64::from(ink.y()) / s, f64::from(ink.width()) / s, f64::from(ink.height()) / s);
    // Scale the ink itself to the square, so a glyph fills it edge to edge.
    let k = size / w.max(h).max(1.0);
    let _ = cr.save();
    cr.translate(size / 2.0, size / 2.0);
    cr.scale(k, k);
    cr.move_to(-x - w / 2.0, -y - h / 2.0);
    pangocairo::functions::show_layout(cr, &layout);
    let _ = cr.restore();
}

fn draw_eclipse(cr: &cairo::Context, size: f64) {
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
