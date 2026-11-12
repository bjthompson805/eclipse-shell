//! The launcher's resting card, shown while the query is empty: a greeting and
//! large clock on the left, the weather on the right, and a drifting horizon
//! along the bottom whose lit stretch shows how far through the day (or night)
//! it is, with the sun or moon riding it. Behind it all, dimmed, an eclipse:
//! a dark disc in a slowly turning corona over a few faint stars.

use std::f64::consts::PI;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, glib, pango};
use gtk4 as gtk;

use crate::anim::{self, Easing};
use crate::bar::every_second;
use crate::config;
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, DIM, FAINT, MOON_DISC, MOON_GLOW, NIGHT_GHOST, SHEEN, SUBTLE, SUN_GOLD};
use crate::weather::{self, Weather};

const CARD_RADIUS: f64 = config::LAUNCHER_RADIUS - 4.0;
const BREATH_MS: f64 = 620.0;
const DRIFT_MS: f64 = 900.0;

/// Seconds after local midnight.
fn now_seconds() -> i32 {
    glib::DateTime::now_local().map(|t| t.hour() * 3600 + t.minute() * 60 + t.second()).unwrap_or(0)
}

/// `(fraction through the phase, is_day)`, falling back to a 06:00–20:00 day
/// until the weather (and so the real sun times) arrives.
fn sky() -> (f64, bool) {
    let now = now_seconds();
    match weather::current() {
        Some(w) => weather::sky_position(now, w.sunrise, w.sunset),
        None => weather::sky_position(now, 6 * 3600, 20 * 3600),
    }
}

pub fn height(s: f64) -> f64 {
    106.0 * s
}

pub fn dashboard(s: f64) -> gtk::Overlay {
    let canvas = gtk::DrawingArea::new();
    canvas.set_size_request(-1, height(s).round() as i32);
    canvas.set_draw_func(move |_, cr, w, h| draw(cr, f64::from(w), f64::from(h), s));
    // The horizon drifts only while the launcher is on screen.
    anim::while_mapped(&canvas, |canvas, _| canvas.queue_draw());

    let widget = gtk::Overlay::new();
    widget.set_child(Some(&canvas));
    widget.add_overlay(&clock_column(s));
    widget.add_overlay(&weather_column(s));
    widget
}

fn clock_column(s: f64) -> gtk::Box {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing((4.0 * s).round() as i32)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start((16.0 * s).round() as i32)
        .margin_top((16.0 * s).round() as i32)
        .build();

    let greeting = TextStyle::new(9.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("");
    greeting.set_halign(gtk::Align::Start);
    greeting.set_visible(config::LAUNCHER_GREETING);

    let digits = TextStyle::new(34.0 * s, pango::Weight::Medium, BRIGHT).mono();
    let hours = digits.label("");
    let minutes = digits.label("");
    let colon = TextStyle { color: sky_colour(sky().1), ..digits }.label(":");
    let meridiem = TextStyle::new(12.0 * s, pango::Weight::Semibold, DIM).mono().tracking(1.0).label("");
    meridiem.set_valign(gtk::Align::Start);
    meridiem.set_margin_start((5.0 * s).round() as i32);
    meridiem.set_margin_top((22.0 * s).round() as i32);
    meridiem.set_visible(!config::CLOCK_24H);

    let time = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for part in [&hours, &colon, &minutes, &meridiem] {
        part.set_valign(gtk::Align::Start);
        time.append(part);
    }
    column.append(&greeting);
    column.append(&time);

    // The colon breathes in the sky colour while the launcher is open.
    anim::while_mapped(&colon, |colon, ms| {
        let phase = (ms / BREATH_MS) % 2.0;
        let t = Easing::InOutSine.apply(if phase < 1.0 { phase } else { 2.0 - phase });
        colon.set_opacity(1.0 - 0.7 * t);
    });

    every_second(&column, move |now| {
        let hour = now.hour();
        let shown_hour = if config::CLOCK_24H { hour } else { (hour + 11) % 12 + 1 };
        hours.set_text(&format!("{shown_hour:02}"));
        minutes.set_text(&format!("{:02}", now.minute()));
        meridiem.set_text(if hour < 12 { "AM" } else { "PM" });
        greeting.set_text(match hour {
            0..=4 => "GOOD NIGHT",
            5..=11 => "GOOD MORNING",
            12..=17 => "GOOD AFTERNOON",
            _ => "GOOD EVENING",
        });
        TextStyle { color: sky_colour(sky().1), ..digits }.apply(&colon);
    });
    column
}

fn weather_column(s: f64) -> gtk::Box {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing((3.0 * s).round() as i32)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_end((16.0 * s).round() as i32)
        .margin_top((18.0 * s).round() as i32)
        .build();

    let glyph_px = (22.0 * s).round() as i32;
    let glyph = gtk::Image::builder().pixel_size(glyph_px).valign(gtk::Align::Center).build();
    let temp = TextStyle::new(22.0 * s, pango::Weight::Medium, BRIGHT).label("");
    let headline = gtk::Box::builder().spacing((6.0 * s).round() as i32).halign(gtk::Align::End).build();
    headline.append(&glyph);
    headline.append(&temp);

    // Until the weather arrives the date takes the headline slot, so the
    // column never sits empty.
    let date_headline = TextStyle::new(22.0 * s, pango::Weight::Medium, BRIGHT).label("");
    date_headline.set_halign(gtk::Align::End);
    let place = TextStyle::new(10.0 * s, pango::Weight::Normal, SUBTLE).label("");
    place.set_halign(gtk::Align::End);
    let date = TextStyle::new(9.0 * s, pango::Weight::Normal, FAINT).label("");
    date.set_halign(gtk::Align::End);

    for part in [headline.upcast_ref::<gtk::Widget>(), date_headline.upcast_ref(), place.upcast_ref(), date.upcast_ref()] {
        column.append(part);
    }

    let refresh = Rc::new({
        let (glyph, temp, date_headline, place, date, headline) = (glyph.clone(), temp.clone(), date_headline.clone(), place.clone(), date.clone(), headline.clone());
        move || {
            let today = glib::DateTime::now_local().ok().and_then(|t| t.format("%A, %b %-d").ok()).map(String::from).unwrap_or_default();
            let shown: Option<Weather> = weather::current().filter(|_| config::LAUNCHER_WEATHER);
            headline.set_visible(shown.is_some());
            place.set_visible(shown.is_some());
            date.set_visible(shown.is_some());
            date_headline.set_visible(shown.is_none());
            date_headline.set_text(&today);
            date.set_text(&today);
            if let Some(w) = shown {
                temp.set_text(&w.temp);
                place.set_text(&if w.city.is_empty() { w.condition.to_owned() } else { format!("{}  ·  {}", w.condition, w.city) });
                glyph.set_paintable(weather::glyph_texture(w.glyph, glyph_px * 3, &CREAM.to_hex(), 1.7).as_ref());
            }
        }
    });
    refresh();
    weather::on_change({
        let refresh = refresh.clone();
        move || refresh()
    });
    column.connect_map(move |_| refresh());
    column
}

fn sky_colour(is_day: bool) -> Rgba {
    if is_day { SUN_GOLD } else { MOON_GLOW }
}

fn draw(cr: &cairo::Context, w: f64, h: f64, s: f64) {
    let palette = theme::palette();
    // Inset half a pixel so the 1px outline lands on whole pixels.
    let outline = || theme::squircle(cr, 0.5, 0.5, w - 1.0, h - 1.0, CARD_RADIUS);
    outline();
    theme::set_source(cr, palette.recessed);
    let _ = cr.fill_preserve();
    let _ = cr.save();
    cr.clip();

    // A lit hairline along the top edge, between the corners.
    cr.rectangle(CARD_RADIUS * s, 1.0, w - 2.0 * CARD_RADIUS * s, 1.0);
    theme::set_source(cr, SHEEN);
    let _ = cr.fill();

    draw_eclipse(cr, w, h, s);
    draw_horizon(cr, w, h, s);
    let _ = cr.restore();

    outline();
    cr.set_line_width(1.0);
    theme::set_source(cr, palette.outline);
    let _ = cr.stroke();
}

/// How strongly the eclipse shows; it's atmosphere behind the text.
const ECLIPSE_STRENGTH: f64 = 0.6;
/// One turn of the corona, in seconds.
const CORONA_TURN_S: f64 = 240.0;

/// A small fixed hash of `i` into 0..1, for star places and streamer lengths
/// that stay put from frame to frame.
fn scatter(i: u32, salt: u32) -> f64 {
    let mut x = i.wrapping_mul(0x9e37_79b9) ^ salt.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2c1b_3c6d);
    x ^= x >> 12;
    f64::from(x % 10_000) / 10_000.0
}

/// The backdrop: stars, then the corona and its streamers, the dark disc,
/// a lit rim and a bead of light where the sun breaks through.
fn draw_eclipse(cr: &cairo::Context, w: f64, h: f64, s: f64) {
    let palette = theme::palette();
    let seconds = glib::monotonic_time() as f64 / 1_000_000.0;
    let (cx, cy) = (w * 0.56, h * 0.42);
    let r = 21.0 * s;
    let (glow, edge) = (palette.accent_lit, palette.edge);

    cr.push_group();

    // Stars, twinkling a little, kept clear of the corona.
    for i in 0..34 {
        let (x, y) = (scatter(i, 1) * w, scatter(i, 2) * h * 0.78);
        if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() < r * 2.6 {
            continue;
        }
        let twinkle = 0.55 + 0.45 * (seconds * (0.6 + scatter(i, 3)) + scatter(i, 4) * 6.0).sin();
        cr.arc(x, y, (0.5 + scatter(i, 5) * 0.8) * s, 0.0, 2.0 * PI);
        theme::set_source(cr, CREAM.alpha(0.35 * twinkle));
        let _ = cr.fill();
    }

    // The corona's soft body.
    let body = cairo::RadialGradient::new(cx, cy, r * 0.9, cx, cy, r * 3.4);
    body.add_color_stop_rgba(0.0, glow.r, glow.g, glow.b, 0.55);
    body.add_color_stop_rgba(0.35, edge.r, edge.g, edge.b, 0.18);
    body.add_color_stop_rgba(1.0, edge.r, edge.g, edge.b, 0.0);
    cr.arc(cx, cy, r * 3.4, 0.0, 2.0 * PI);
    let _ = cr.set_source(&body);
    let _ = cr.fill();

    // Streamers: tapered petals of fixed lengths, turning slowly.
    let turn = seconds / CORONA_TURN_S * 2.0 * PI;
    for i in 0..14 {
        let angle = f64::from(i) / 14.0 * 2.0 * PI + scatter(i, 6) * 0.35 + turn;
        let reach = r * (1.5 + scatter(i, 7) * 1.7);
        let spread = 0.07 + scatter(i, 8) * 0.07;
        let at = |a: f64, d: f64| (cx + a.cos() * d, cy + a.sin() * d);
        let (bx0, by0) = at(angle - spread, r * 0.95);
        let (bx1, by1) = at(angle + spread, r * 0.95);
        let (tx, ty) = at(angle, reach);
        let (c0x, c0y) = at(angle - spread * 0.4, reach * 0.62);
        let (c1x, c1y) = at(angle + spread * 0.4, reach * 0.62);
        cr.move_to(bx0, by0);
        cr.curve_to(c0x, c0y, c0x, c0y, tx, ty);
        cr.curve_to(c1x, c1y, c1x, c1y, bx1, by1);
        cr.close_path();
        let fade = cairo::RadialGradient::new(cx, cy, r, cx, cy, reach);
        fade.add_color_stop_rgba(0.0, glow.r, glow.g, glow.b, 0.32);
        fade.add_color_stop_rgba(1.0, glow.r, glow.g, glow.b, 0.0);
        let _ = cr.set_source(&fade);
        let _ = cr.fill();
    }

    // The disc, its lit rim, and the bead up and to the right, as on the mark.
    cr.arc(cx, cy, r, 0.0, 2.0 * PI);
    theme::set_source(cr, palette.recessed);
    let _ = cr.fill_preserve();
    cr.set_line_width(1.2 * s);
    theme::set_source(cr, glow.alpha(0.85));
    let _ = cr.stroke();
    let bead = -PI / 4.0;
    let (bx, by) = (cx + bead.cos() * r, cy + bead.sin() * r);
    let shine = cairo::RadialGradient::new(bx, by, 0.0, bx, by, 9.0 * s);
    shine.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 0.95);
    shine.add_color_stop_rgba(0.25, glow.r, glow.g, glow.b, 0.7);
    shine.add_color_stop_rgba(1.0, glow.r, glow.g, glow.b, 0.0);
    cr.arc(bx, by, 9.0 * s, 0.0, 2.0 * PI);
    let _ = cr.set_source(&shine);
    let _ = cr.fill();

    let _ = cr.pop_group_to_source();
    let _ = cr.paint_with_alpha(ECLIPSE_STRENGTH);
}

fn gradient(top: f64, bottom: f64, colour: Rgba, stops: &[(f64, f64)]) -> cairo::LinearGradient {
    let g = cairo::LinearGradient::new(0.0, top, 0.0, bottom);
    for &(at, alpha) in stops {
        g.add_color_stop_rgba(at, colour.r, colour.g, colour.b, alpha);
    }
    g
}

fn draw_horizon(cr: &cairo::Context, w: f64, h: f64, s: f64) {
    let (fraction, is_day) = sky();
    let tint = sky_colour(is_day);
    let band = 42.0 * s;
    let top = h - band;
    let baseline = top + band * 0.66;
    let amplitude = 5.0 * s;
    let drift = glib::monotonic_time() as f64 / 1000.0 / DRIFT_MS;
    let ridge = |x: f64| {
        let t = x / w;
        baseline + amplitude * (t * PI * 3.0 + drift).sin() + amplitude * 0.4 * (t * PI * 7.0 - drift * 0.7).sin()
    };
    let marker = (w * fraction).clamp(2.0 * s, w - 2.0 * s);
    const STEPS: i32 = 72;
    let trace = |x0: f64, x1: f64| {
        for i in 0..=STEPS {
            let x = x0 + (x1 - x0) * f64::from(i) / f64::from(STEPS);
            cr.line_to(x, ridge(x));
        }
    };
    let fill_under = |x0: f64, x1: f64, pattern: &cairo::LinearGradient| {
        cr.new_path();
        cr.move_to(x0, h);
        trace(x0, x1);
        cr.line_to(x1, h);
        cr.close_path();
        let _ = cr.set_source(pattern);
        let _ = cr.fill();
    };
    let glow_top = baseline - amplitude * 2.0;

    // Elapsed stretch glows in the sky colour; what's left of the phase is faint.
    fill_under(0.0, marker, &gradient(glow_top, h, tint, &[(0.0, 0.0), (0.5, 0.15), (1.0, 0.36)]));
    fill_under(marker, w, &gradient(glow_top, h, NIGHT_GHOST, &[(0.0, 0.0), (1.0, 0.22)]));

    cr.set_line_width(2.0 * s);
    cr.set_line_cap(cairo::LineCap::Round);
    for (x0, x1, colour) in [(0.0, marker, tint.alpha(0.9)), (marker, w, FAINT.alpha(0.5))] {
        cr.new_path();
        cr.move_to(x0, ridge(x0));
        trace(x0, x1);
        theme::set_source(cr, colour);
        let _ = cr.stroke();
    }

    // The sun, or a crescent moon carved out of a disc.
    let y = ridge(marker);
    let halo = cairo::RadialGradient::new(marker, y, 0.0, marker, y, 15.0 * s);
    halo.add_color_stop_rgba(0.0, tint.r, tint.g, tint.b, 0.5);
    halo.add_color_stop_rgba(1.0, tint.r, tint.g, tint.b, 0.0);
    cr.new_path();
    cr.arc(marker, y, 15.0 * s, 0.0, 2.0 * PI);
    let _ = cr.set_source(&halo);
    let _ = cr.fill();

    let r = 5.5 * s;
    cr.arc(marker, y, r, 0.0, 2.0 * PI);
    theme::set_source(cr, if is_day { SUN_GOLD } else { MOON_DISC.alpha(0.98) });
    let _ = cr.fill();
    if !is_day {
        cr.arc(marker + r * 0.6, y - r * 0.4, r, 0.0, 2.0 * PI);
        theme::set_source(cr, theme::palette().recessed);
        let _ = cr.fill();
    }
}
