//! Now playing: a round art thumbnail, the track line, and the play state.
//! A line too long for its slot slides to its end and back while playing.
//! Hidden while nothing is playing.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::cairo::{Format, ImageSurface};
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::media;
use crate::theme::{self, TextStyle, CREAM, DIM, HAIR, ICON_DIM};

const MAX_TITLE: f64 = 170.0;
const HOLD_MS: f64 = 1800.0;
const MS_PER_PX: f64 = 22.0;

pub fn media(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (11.0 * s).round() as i32);
    let thumb = gtk::DrawingArea::new();
    let thumb_px = (14.0 * s).round() as i32;
    thumb.set_size_request(thumb_px, thumb_px);
    thumb.set_valign(gtk::Align::Center);
    let title = gtk::DrawingArea::new();
    title.set_valign(gtk::Align::Center);
    let state = theme::filled_icon_glyph("pause", 13.0 * s, DIM);
    row.append(&thumb);
    row.append(&title);
    row.append(&state);

    let art: Rc<RefCell<Option<ImageSurface>>> = Rc::default();
    let playing = Rc::new(Cell::new(false));
    thumb.set_draw_func({
        let (art, playing) = (art.clone(), playing.clone());
        move |_, cr, w, h| draw_thumb(cr, f64::from(w), f64::from(h), art.borrow().as_ref(), playing.get(), s)
    });

    let layout = title.create_pango_layout(None);
    let offset = Rc::new(Cell::new(0.0));
    title.set_draw_func({
        let (layout, offset) = (layout.clone(), offset.clone());
        move |_, cr, _, h| {
            let (_, logical) = layout.pixel_extents();
            cr.move_to(-offset.get(), (f64::from(h) - f64::from(logical.height())) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    });
    slide(&title, &layout, &offset, &playing);

    let shown_art = Rc::new(RefCell::new(String::new()));
    let paint = {
        let row = row.downgrade();
        move || {
            let Some(row) = row.upgrade() else { return };
            let Some(player) = media::current() else { return row.set_visible(false) };
            row.set_visible(true);
            playing.set(player.playing);

            let style = TextStyle::new(10.5 * s, pango::Weight::Medium, if player.playing { CREAM } else { DIM });
            layout.set_text(&player.line());
            layout.set_attributes(Some(&style.attrs()));
            let (_, logical) = layout.pixel_extents();
            let width = f64::from(logical.width()).min(MAX_TITLE * s);
            title.set_size_request(width.ceil() as i32 + 2, logical.height());
            title.queue_draw();

            let (name, color) = if player.playing { ("play_arrow", theme::palette().accent) } else { ("pause", DIM) };
            state.set_text(name);
            state.set_attributes(theme::filled_icon_glyph(name, 13.0 * s, color).attributes().as_ref());

            if *shown_art.borrow() != player.art_url {
                *shown_art.borrow_mut() = player.art_url.clone();
                load_art(&player.art_url, &art, &thumb);
            }
            thumb.queue_draw();
        }
    };
    paint();
    media::on_change(paint);
    row
}

pub fn toggle() {
    if let Some(player) = media::current() {
        media::play_pause(&player);
    }
}

/// Ping-pongs the line: hold at the start, slide to the end, hold, slide
/// back. Rests at the start whenever playback stops.
fn slide(area: &gtk::DrawingArea, layout: &pango::Layout, offset: &Rc<Cell<f64>>, playing: &Rc<Cell<bool>>) {
    let (layout, offset, playing) = (layout.clone(), offset.clone(), playing.clone());
    let start = Cell::new(None::<f64>);
    area.add_tick_callback(move |area, clock| {
        let overflow = f64::from(layout.pixel_extents().1.width()) - f64::from(area.width()) + 2.0;
        if !playing.get() || overflow <= 0.0 {
            start.set(None);
            if offset.get() != 0.0 {
                offset.set(0.0);
                area.queue_draw();
            }
            return glib::ControlFlow::Continue;
        }
        let now = clock.frame_time() as f64 / 1000.0;
        let t = now - start.get().unwrap_or(now);
        if start.get().is_none() {
            start.set(Some(now));
        }
        let travel = overflow * MS_PER_PX;
        let cycle = 2.0 * (HOLD_MS + travel);
        let phase = t % cycle;
        let x = if phase < HOLD_MS {
            0.0
        } else if phase < HOLD_MS + travel {
            (phase - HOLD_MS) / travel
        } else if phase < 2.0 * HOLD_MS + travel {
            1.0
        } else {
            1.0 - (phase - 2.0 * HOLD_MS - travel) / travel
        };
        offset.set(x * overflow);
        area.queue_draw();
        glib::ControlFlow::Continue
    });
}

/// A circle with the cover art cropped to fill it, or a note while there
/// is none; the rim takes the accent while playing.
fn draw_thumb(cr: &gtk::cairo::Context, w: f64, h: f64, art: Option<&ImageSurface>, playing: bool, s: f64) {
    let (cx, cy, r) = (w / 2.0, h / 2.0, w.min(h) / 2.0);
    cr.arc(cx, cy, r, 0.0, 2.0 * PI);
    theme::set_source(cr, theme::BRIGHT.alpha(0.05));
    let _ = cr.fill();

    match art {
        Some(surface) => {
            let _ = cr.save();
            cr.arc(cx, cy, r - 1.0, 0.0, 2.0 * PI);
            cr.clip();
            let (tw, th) = (f64::from(surface.width()), f64::from(surface.height()));
            let scale = (2.0 * r) / tw.min(th);
            cr.translate(cx - tw * scale / 2.0, cy - th * scale / 2.0);
            cr.scale(scale, scale);
            let _ = cr.set_source_surface(surface, 0.0, 0.0);
            cr.source().set_filter(gtk::cairo::Filter::Good);
            let _ = cr.paint();
            let _ = cr.restore();
        }
        None => {
            let layout = pangocairo::functions::create_layout(cr);
            let style = TextStyle { family: crate::config::ICON_FONT, ..TextStyle::new(10.0 * s, pango::Weight::Medium, ICON_DIM) };
            layout.set_text("music_note");
            layout.set_attributes(Some(&style.attrs()));
            let (_, logical) = layout.pixel_extents();
            cr.move_to(cx - f64::from(logical.width()) / 2.0, cy - f64::from(logical.height()) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }

    cr.arc(cx, cy, r - 0.5, 0.0, 2.0 * PI);
    cr.set_line_width(1.0);
    theme::set_source(cr, if playing { theme::palette().accent.alpha(0.8) } else { HAIR });
    let _ = cr.stroke();
}

fn load_art(url: &str, art: &Rc<RefCell<Option<ImageSurface>>>, thumb: &gtk::DrawingArea) {
    *art.borrow_mut() = None;
    if url.is_empty() {
        return;
    }
    let (url, art, thumb) = (url.to_owned(), art.clone(), thumb.downgrade());
    glib::spawn_future_local(async move {
        let Ok((bytes, _)) = gio::File::for_uri(&url).load_bytes_future().await else { return };
        let Ok(texture) = gdk::Texture::from_bytes(&bytes) else { return };
        let Some(surface) = to_surface(&texture) else { return };
        *art.borrow_mut() = Some(surface);
        if let Some(thumb) = thumb.upgrade() {
            thumb.queue_draw();
        }
    });
}

/// Copies a texture into a cairo surface (premultiplied BGRA, which is
/// cairo's ARGB32 on little-endian machines).
fn to_surface(texture: &gdk::Texture) -> Option<ImageSurface> {
    let (w, h) = (texture.width(), texture.height());
    let mut surface = ImageSurface::create(Format::ARgb32, w, h).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        texture.download(&mut data, stride);
    }
    surface.mark_dirty();
    Some(surface)
}
