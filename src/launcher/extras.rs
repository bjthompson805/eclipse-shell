//! Extras around the resting launcher: a now-playing card under the
//! dashboard while a player has a track, and a bubble per connected
//! Bluetooth device under the card.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, BRIGHT, CREAM, DIM, HAIR, ICON_DIM};
use crate::{bluetooth, media, popout};

/// The now-playing card. It fills itself from the current player, keeps up
/// with changes, and hides while nothing has a track.
pub fn now_playing(s: f64) -> gtk::Box {
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let art: Rc<RefCell<(String, Option<cairo::ImageSurface>)>> = Rc::default();
    let fill = {
        let (holder, art) = (holder.downgrade(), art.clone());
        Rc::new(move || {
            let Some(holder) = holder.upgrade() else { return };
            fill_card(&holder, s, &art);
        })
    };
    fill();
    media::on_change({
        let fill = fill.clone();
        move || fill()
    });
    holder
}

fn fill_card(holder: &gtk::Box, s: f64, art: &Rc<RefCell<(String, Option<cairo::ImageSurface>)>>) {
    let px = |v: f64| (v * s).round() as i32;
    while let Some(child) = holder.first_child() {
        holder.remove(&child);
    }
    let Some(player) = media::current().filter(|p| !p.title.is_empty()) else {
        holder.set_visible(false);
        return;
    };
    holder.set_visible(true);

    let cover = gtk::DrawingArea::builder().content_width(px(44.0)).content_height(px(44.0)).build();
    cover.set_draw_func({
        let art = art.clone();
        move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.0, 0.0, w, h, 7.0 * s);
            cr.clip();
            theme::set_source(cr, theme::palette().raised);
            let _ = cr.paint();
            if let Some(image) = &art.borrow().1 {
                let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
                let k = (w / iw).max(h / ih);
                cr.translate((w - iw * k) / 2.0, (h - ih * k) / 2.0);
                cr.scale(k, k);
                let _ = cr.set_source_surface(image, 0.0, 0.0);
                let _ = cr.paint();
            }
        }
    });
    if art.borrow().0 != player.art_url {
        art.borrow_mut().0 = player.art_url.clone();
        art.borrow_mut().1 = None;
        if !player.art_url.is_empty() {
            let (art, url, cover) = (art.clone(), player.art_url.clone(), cover.downgrade());
            glib::spawn_future_local(async move {
                let surface = match gio::File::for_uri(&url).load_bytes_future().await {
                    Ok((bytes, _)) => gdk::Texture::from_bytes(&bytes).ok().and_then(|t| theme::texture_surface(&t)),
                    Err(_) => None,
                };
                if art.borrow().0 == url {
                    art.borrow_mut().1 = surface;
                    cover.upgrade().into_iter().for_each(|c| c.queue_draw());
                }
            });
        }
    }

    let text = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let title = TextStyle::new(11.5 * s, pango::Weight::Semibold, BRIGHT).label(&player.title);
    title.set_xalign(0.0);
    title.set_ellipsize(pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    text.append(&title);
    let by = if player.artists.is_empty() { player.identity.clone() } else { format!("{} · {}", player.artists, player.identity) };
    let artist = TextStyle::new(10.0 * s, pango::Weight::Normal, DIM).label(&by);
    artist.set_xalign(0.0);
    artist.set_ellipsize(pango::EllipsizeMode::End);
    artist.set_max_width_chars(1);
    text.append(&artist);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, px(4.0));
    controls.set_valign(gtk::Align::Center);
    let button = |glyph: &str, enabled: bool, pressed: fn(&media::Player)| {
        let label = theme::filled_icon_glyph(glyph, 20.0 * s, if enabled { CREAM } else { ICON_DIM.alpha(0.4) });
        label.set_size_request(px(30.0), px(30.0));
        if enabled {
            let p = player.clone();
            popout::on_click(&label, move || pressed(&p));
        }
        label
    };
    controls.append(&button("skip_previous", player.can_previous, media::previous));
    controls.append(&button(if player.playing { "pause" } else { "play_arrow" }, true, media::play_pause));
    controls.append(&button("skip_next", player.can_next, media::next));

    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(12.0));
    row.set_margin_start(px(10.0));
    row.set_margin_end(px(10.0));
    row.set_margin_top(px(8.0));
    row.set_margin_bottom(px(8.0));
    row.append(&cover);
    row.append(&text);
    row.append(&controls);
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 11.0 * s);
        theme::set_source(cr, palette.recessed);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, HAIR);
        let _ = cr.stroke();
    });
    let card = gtk::Overlay::new();
    card.set_child(Some(&back));
    card.add_overlay(&row);
    card.set_measure_overlay(&row, true);
    holder.append(&card);
}

/// The Bluetooth bubbles, empty until [`fill_bubbles`].
pub fn bubbles(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (10.0 * s).round() as i32);
    row.set_halign(gtk::Align::Center);
    row.set_valign(gtk::Align::Start);
    row
}

/// Reads the connected devices (off the main thread) and shows a bubble each.
pub fn fill_bubbles(row: &gtk::Box, s: f64) {
    let row = row.downgrade();
    glib::spawn_future_local(async move {
        let devices = gio::spawn_blocking(|| bluetooth::read().devices.into_iter().filter(|d| d.connected).collect::<Vec<_>>()).await.unwrap_or_default();
        let Some(row) = row.upgrade() else { return };
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
        for device in devices {
            row.append(&bubble(s, device.glyph, &device.name));
        }
    });
}

fn bubble(s: f64, glyph: &str, name: &str) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let column = gtk::Box::new(gtk::Orientation::Vertical, px(5.0));
    column.set_valign(gtk::Align::Center);
    column.append(&theme::filled_icon_glyph(glyph, 22.0 * s, theme::palette().accent_lit));
    let label = TextStyle::new(9.0 * s, pango::Weight::Medium, CREAM).label(name);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    label.set_margin_start(px(6.0));
    label.set_margin_end(px(6.0));
    column.append(&label);
    let back = gtk::DrawingArea::new();
    back.set_size_request(px(76.0), px(76.0));
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        theme::squircle(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 16.0 * s);
        theme::set_source(cr, palette.base);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
    });
    let bubble = gtk::Overlay::new();
    bubble.set_child(Some(&back));
    bubble.add_overlay(&column);
    popout::on_click(&bubble, || {
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "eclipse-shell".into());
        crate::audio::spawn(&[&exe, "toggle-bluetooth"]);
    });
    bubble
}
