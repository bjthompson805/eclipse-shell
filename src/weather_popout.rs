//! Weather popout, dropping from the weather pill: the temperature and how it
//! feels, wind, humidity, rain and UV; the next twelve hours as a temperature
//! curve over the chance of rain; the week as ranges against the week's own
//! extremes; and the sun's arc between sunrise and sunset.
//!
//! The forecast comes from src/weather.rs, which keeps it for ten minutes.

use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};
use gtk4 as gtk;

use crate::hint;
use crate::popout::{self, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, FLAME, HAIR, ICON_DIM, MOON_GLOW, SUBTLE, SUN_GOLD};
use crate::config;
use crate::weather::{self, Day, Forecast, Hour};

const NAMESPACE: &str = "eclipse-shell-weather";
const WIDTH: f64 = 340.0;
const WEEK_HELP: &str = "Each bar runs from the day's low to its high, on one scale for the whole week: \
the left end of the track is the week's coldest, the right end its warmest. A bar further right is a warmer day; \
a longer one swings more between night and day.\n\nThe colour follows the temperature, blue for cold to orange \
for warm, the same on every row. The ring on today's bar is the temperature now, and the blue percentages are \
the chance of rain.";
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub struct WeatherPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a forecast that arrives after a close is dropped.
    generation: Cell<u32>,
}

impl WeatherPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Self::with_popout(Popout::new(app, NAMESPACE))
    }

    /// Shown on `host`, e.g. a [`Popout::pane`] in a sidebar.
    pub fn with_popout(host: Rc<Popout>) -> Rc<Self> {
        Rc::new(Self { popout: host, generation: Cell::new(0) })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let popout = self.clone();
        glib::spawn_future_local(async move {
            let forecast = weather::forecast().await;
            if popout.generation.get() != generation {
                return;
            }
            let s = popout.popout.scale();
            let body = match forecast {
                Ok(f) => body(s, &f, popout.popout.hints()),
                Err(why) => unavailable(s, &why.to_string()),
            };
            popout.popout.set_content(&popout::card(&body));
            popout.popout.show(Anchor::Under(x));
        });
    }
}

fn column(s: f64) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(px(12.0))
        .width_request(px(WIDTH) - 2 * px(16.0))
        .margin_top(px(13.0))
        .margin_bottom(px(14.0))
        .margin_start(px(16.0))
        .margin_end(px(16.0))
        .build()
}

fn header(s: f64, glyph: &str, city: &str) -> gtk::Box {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, (8.0 * s).round() as i32);
    head.append(&theme::filled_icon_glyph(glyph, 16.0 * s, theme::palette().accent));
    let title = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("WEATHER");
    title.set_hexpand(true);
    title.set_xalign(0.0);
    head.append(&title);
    head.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(city));
    head
}

fn unavailable(s: f64, why: &str) -> gtk::Box {
    let column = column(s);
    column.append(&header(s, "cloud_off", ""));
    let note = TextStyle::new(10.5 * s, pango::Weight::Normal, FAINT).label(&format!("Weather is unavailable: {why}"));
    note.set_xalign(0.0);
    note.set_wrap(true);
    column.append(&note);
    column
}

/// `hints` is where hover hints float.
fn body(s: f64, f: &Forecast, hints: &gtk::Fixed) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let column = column(s);
    column.append(&header(s, f.now.glyph.symbol_at(f.now.day), &f.city));
    let today = f.days.first();

    // The temperature, and what it means.
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, px(14.0));
    let temp = TextStyle::new(38.0 * s, pango::Weight::Semibold, CREAM).tracking(-1.0 * s).label(&format!("{}{}", f.now.temp.round(), f.unit));
    hero.append(&temp);
    let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(px(2.0)).valign(gtk::Align::Center).build();
    let condition = TextStyle::new(13.0 * s, pango::Weight::Semibold, CREAM).label(f.now.condition);
    condition.set_xalign(0.0);
    words.append(&condition);
    let mut detail = format!("Feels like {}", degrees(f.now.feels_like));
    if let Some(d) = today {
        detail += &format!(" \u{b7} H {} L {}", degrees(d.high), degrees(d.low));
    }
    let detail = TextStyle::new(10.5 * s, pango::Weight::Medium, SUBTLE).label(&detail);
    detail.set_xalign(0.0);
    words.append(&detail);
    hero.append(&words);
    column.append(&hero);

    let chips = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
    chips.append(&chip(s, "air", &format!("{} {} {}", f.now.wind.round(), f.wind_unit, compass(f.now.wind_from))));
    chips.append(&chip(s, "humidity_percentage", &format!("{}%", f.now.humidity.round())));
    if let Some(d) = today {
        chips.append(&chip(s, "umbrella", &format!("{}%", d.rain.round())));
        chips.append(&chip(s, "wb_sunny", &format!("UV {}", d.uv.round())));
    }
    column.append(&chips);

    column.append(&popout::divider());
    column.append(&popout::micro_label(s, "Next 12 hours"));
    column.append(&hourly(s, &f.hours));

    column.append(&popout::divider());
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
    heading.append(&popout::micro_label(s, "This week"));
    heading.append(&info(s, hints, WEEK_HELP));
    column.append(&heading);
    let (week_low, week_high) = f.days.iter().fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(d.low), hi.max(d.high)));
    let days = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    for (i, day) in f.days.iter().enumerate() {
        let now = (i == 0).then_some(f.now.temp);
        days.append(&day_row(s, day, i == 0, now, (week_low, week_high)));
    }
    column.append(&days);

    if let Some(d) = today {
        column.append(&popout::divider());
        column.append(&sun_arc(s, d, f.now.seconds));
    }
    column
}

/// An info glyph that shows `help` while hovered.
fn info(s: f64, hints: &gtk::Fixed, help: &'static str) -> gtk::Label {
    let glyph = theme::icon_glyph("info", 13.0 * s, ICON_DIM);
    glyph.set_valign(gtk::Align::Center);
    on_hover(&glyph, {
        let glyph = glyph.downgrade();
        move |inside| {
            if let Some(g) = glyph.upgrade() {
                g.set_attributes(theme::icon_glyph("info", 13.0 * s, if inside { CREAM } else { ICON_DIM }).attributes().as_ref());
            }
        }
    });
    hint::float_below(&glyph, hints, s, move || hint::note(s, help, 260.0));
    glyph
}

/// "46°"; the big temperature alone carries the unit.
fn degrees(t: f64) -> String {
    format!("{}°", t.round())
}

fn compass(from: f64) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    POINTS[((from.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

/// "6:56 AM", or "06:56" on a 24-hour clock.
fn clock(seconds: i32) -> String {
    let (h, m) = (seconds / 3600, seconds / 60 % 60);
    if config::get().clock_24h {
        return format!("{h:02}:{m:02}");
    }
    format!("{}:{m:02} {}", (h + 11) % 12 + 1, if h < 12 { "AM" } else { "PM" })
}

/// "3p", "12a", or "15" on a 24-hour clock.
fn hour_label(hour: u32) -> String {
    if config::get().clock_24h {
        return format!("{hour:02}");
    }
    format!("{}{}", (hour + 11) % 12 + 1, if hour < 12 { "a" } else { "p" })
}

/// A glyph and a reading on a small outlined pill.
fn chip(s: f64, glyph: &str, text: &str) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let content = gtk::Box::builder().spacing(px(5.0)).margin_start(px(8.0)).margin_end(px(9.0)).margin_top(px(4.0)).margin_bottom(px(4.0)).build();
    content.append(&theme::icon_glyph(glyph, 13.0 * s, ICON_DIM));
    content.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, SUBTLE).label(text));
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
    });
    let chip = gtk::Overlay::new();
    chip.set_child(Some(&back));
    chip.add_overlay(&content);
    chip.set_measure_overlay(&content, true);
    chip
}

// ---- the next hours ----------------------------------------------------------

/// Columns of hour, glyph and temperature, the temperatures joined by a
/// smooth curve, with the chance of rain as bars along the bottom.
fn hourly(s: f64, hours: &[Hour]) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().height_request((118.0 * s).round() as i32).hexpand(true).build();
    let hours = hours.to_vec();
    area.set_draw_func(move |_, cr, w, _| draw_hourly(cr, f64::from(w), &hours, s));
    area
}

fn draw_hourly(cr: &cairo::Context, w: f64, hours: &[Hour], s: f64) {
    if hours.is_empty() {
        return;
    }
    let palette = theme::palette();
    let slot = w / hours.len() as f64;
    let centre = |i: usize| slot * (i as f64 + 0.5);
    let (lo, hi) = hours.iter().fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.temp), hi.max(h.temp)));
    // The curve's band, below the glyphs and the labels riding on it.
    let (top, bottom) = (58.0 * s, 86.0 * s);
    let y_of = |t: f64| if hi > lo { bottom - (t - lo) / (hi - lo) * (bottom - top) } else { (top + bottom) / 2.0 };
    let points: Vec<(f64, f64)> = hours.iter().enumerate().map(|(i, h)| (centre(i), y_of(h.temp))).collect();

    // Rain chances along the bottom.
    let (floor, reach) = (118.0 * s, 16.0 * s);
    for (i, h) in hours.iter().enumerate() {
        if h.rain >= 1.0 {
            let bar = (h.rain / 100.0 * reach).max(2.0 * s);
            let bw = slot * 0.36;
            theme::rounded_rect(cr, centre(i) - bw / 2.0, floor - bar, bw, bar, 2.0 * s);
            theme::set_source(cr, MOON_GLOW.alpha(0.25 + 0.5 * h.rain / 100.0));
            let _ = cr.fill();
        }
    }

    // A wash under the curve, then the curve.
    smooth_path(cr, &points);
    cr.line_to(points.last().map_or(w, |p| p.0), floor - reach - 2.0 * s);
    cr.line_to(points[0].0, floor - reach - 2.0 * s);
    cr.close_path();
    let wash = cairo::LinearGradient::new(0.0, top, 0.0, floor - reach);
    wash.add_color_stop_rgba(0.0, palette.accent.r, palette.accent.g, palette.accent.b, 0.22);
    wash.add_color_stop_rgba(1.0, palette.accent.r, palette.accent.g, palette.accent.b, 0.0);
    let _ = cr.set_source(&wash);
    let _ = cr.fill();
    smooth_path(cr, &points);
    cr.set_line_width(2.0 * s);
    cr.set_line_cap(cairo::LineCap::Round);
    theme::set_source(cr, palette.accent_lit);
    let _ = cr.stroke();

    for (i, (h, &(x, y))) in hours.iter().zip(&points).enumerate() {
        let now = i == 0;
        let hour = if now { "Now".to_owned() } else { hour_label(h.hour) };
        text(cr, &hour, TextStyle::new(9.5 * s, pango::Weight::Semibold, if now { CREAM } else { DIM }).mono(), x, 0.0);
        let glyph = TextStyle { family: config::get().icon_font.as_str(), ..TextStyle::new(16.0 * s, pango::Weight::Medium, if h.day { SUBTLE } else { ICON_DIM }) };
        text(cr, h.glyph.symbol_at(h.day), glyph, x, 17.0 * s);
        text(cr, &degrees(h.temp), TextStyle::new(10.0 * s, pango::Weight::Semibold, CREAM), x, y - 18.0 * s);
        cr.arc(x, y, if now { 3.5 * s } else { 2.5 * s }, 0.0, 2.0 * PI);
        theme::set_source(cr, if now { CREAM } else { palette.accent_lit });
        let _ = cr.fill();
    }
}

/// A curve through `points` that bends smoothly at each (Catmull-Rom as
/// Béziers, with the ends held level).
fn smooth_path(cr: &cairo::Context, points: &[(f64, f64)]) {
    cr.move_to(points[0].0, points[0].1);
    for i in 0..points.len() - 1 {
        let p0 = points[i.saturating_sub(1)];
        let (p1, p2) = (points[i], points[i + 1]);
        let p3 = points[(i + 2).min(points.len() - 1)];
        let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0);
        let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0);
        cr.curve_to(c1.0, c1.1, c2.0, c2.1, p2.0, p2.1);
    }
}

/// Text centred on `x`, its top at `y`.
fn text(cr: &cairo::Context, s: &str, style: TextStyle, x: f64, y: f64) {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text(s);
    let attrs = style.attrs();
    if style.family == config::get().icon_font.as_str() {
        let mut font = style.font();
        font.set_variations(Some("FILL=1,opsz=20"));
        attrs.insert(pango::AttrFontDesc::new(&font));
    }
    layout.set_attributes(Some(&attrs));
    let (_, logical) = layout.pixel_extents();
    cr.move_to(x - f64::from(logical.width()) / 2.0, y);
    pangocairo::functions::show_layout(cr, &layout);
}

// ---- the week ----------------------------------------------------------------

/// A day: its name, glyph and chance of rain, then its low to high as a bar
/// placed against the whole week's range.
fn day_row(s: f64, day: &Day, today: bool, now: Option<f64>, week: (f64, f64)) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let row = gtk::Box::builder().spacing(px(10.0)).height_request(px(26.0)).build();
    let name = TextStyle::new(11.0 * s, if today { pango::Weight::Semibold } else { pango::Weight::Medium }, if today { CREAM } else { SUBTLE })
        .label(if today { "Today" } else { WEEKDAYS[day.weekday as usize % 7] });
    name.set_xalign(0.0);
    name.set_size_request(px(44.0), -1);
    row.append(&name);
    row.append(&theme::filled_icon_glyph(day.glyph.symbol(), 15.0 * s, SUBTLE));
    let rain = TextStyle::new(9.5 * s, pango::Weight::Semibold, MOON_GLOW).label(&if day.rain >= 10.0 { format!("{}%", day.rain.round()) } else { String::new() });
    rain.set_size_request(px(30.0), -1);
    rain.set_xalign(0.0);
    row.append(&rain);
    let low = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label(&degrees(day.low));
    low.set_size_request(px(30.0), -1);
    low.set_xalign(1.0);
    row.append(&low);

    let bar = gtk::DrawingArea::builder().hexpand(true).build();
    let (lo, hi) = (day.low, day.high);
    bar.set_draw_func(move |_, cr, w, h| draw_range(cr, f64::from(w), f64::from(h), (lo, hi), now, week, s));
    row.append(&bar);

    let high = TextStyle::new(10.5 * s, pango::Weight::Semibold, CREAM).label(&degrees(day.high));
    high.set_size_request(px(30.0), -1);
    high.set_xalign(1.0);
    row.append(&high);
    row
}

/// The week's range as a track; the day's part of it filled from cool to
/// warm; today's reading as a ring.
fn draw_range(cr: &cairo::Context, w: f64, h: f64, (lo, hi): (f64, f64), now: Option<f64>, (week_lo, week_hi): (f64, f64), s: f64) {
    let thick = 5.0 * s;
    let y = (h - thick) / 2.0;
    let span = (week_hi - week_lo).max(1.0);
    let x_of = |t: f64| ((t - week_lo) / span).clamp(0.0, 1.0) * w;
    theme::rounded_rect(cr, 0.0, y, w, thick, thick / 2.0);
    theme::set_source(cr, theme::BRIGHT.alpha(0.07));
    let _ = cr.fill();

    // Colour by temperature across the week, so the same warmth reads the
    // same on every row.
    let warmth = cairo::LinearGradient::new(0.0, 0.0, w, 0.0);
    for (at, c) in [(0.0, MOON_GLOW), (0.55, SUN_GOLD), (1.0, FLAME)] {
        warmth.add_color_stop_rgba(at, c.r, c.g, c.b, 1.0);
    }
    let (x0, x1) = (x_of(lo), x_of(hi).max(x_of(lo) + thick));
    theme::rounded_rect(cr, x0, y, x1 - x0, thick, thick / 2.0);
    let _ = cr.set_source(&warmth);
    let _ = cr.fill();

    if let Some(t) = now {
        let x = x_of(t).clamp(thick, (w - thick).max(thick));
        cr.arc(x, h / 2.0, thick * 0.9, 0.0, 2.0 * PI);
        theme::set_source(cr, theme::palette().base);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.6 * s);
        theme::set_source(cr, CREAM);
        let _ = cr.stroke();
    }
}

// ---- the sun -----------------------------------------------------------------

/// The day as an arc from sunrise to sunset, travelled so far in gold, the
/// sun on it while it's up.
fn sun_arc(s: f64, day: &Day, now: i32) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let view = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    let arc = gtk::DrawingArea::builder().height_request(px(46.0)).hexpand(true).build();
    let (rise, set) = (day.sunrise, day.sunset);
    arc.set_draw_func(move |_, cr, w, h| draw_sun_arc(cr, f64::from(w), f64::from(h), rise, set, now, s));
    view.append(&arc);

    let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for (glyph, label, at, align) in [("wb_twilight", "Sunrise", rise, gtk::Align::Start), ("bedtime", "Sunset", set, gtk::Align::End)] {
        let side = gtk::Box::builder().spacing(px(5.0)).halign(align).hexpand(true).build();
        side.append(&theme::icon_glyph(glyph, 13.0 * s, ICON_DIM));
        side.append(&TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).label(label));
        side.append(&TextStyle::new(10.5 * s, pango::Weight::Semibold, CREAM).label(&clock(at)));
        times.append(&side);
    }
    view.append(&times);
    view
}

fn draw_sun_arc(cr: &cairo::Context, w: f64, h: f64, rise: i32, set: i32, now: i32, s: f64) {
    let (inset, base) = (14.0 * s, h - 4.0 * s);
    let (cx, rx, ry) = (w / 2.0, w / 2.0 - inset, h - 10.0 * s);
    let point = |t: f64| (cx - rx * (PI * t).cos(), base - ry * (PI * t).sin());

    // The horizon.
    cr.move_to(0.0, base);
    cr.line_to(w, base);
    cr.set_line_width(1.0);
    theme::set_source(cr, HAIR);
    let _ = cr.stroke();

    let trace = |cr: &cairo::Context, from: f64, to: f64| {
        let steps = 48;
        for i in 0..=steps {
            let (x, y) = point(from + (to - from) * f64::from(i) / f64::from(steps));
            if i == 0 { cr.move_to(x, y) } else { cr.line_to(x, y) }
        }
    };
    cr.set_dash(&[3.0 * s, 4.0 * s], 0.0);
    trace(cr, 0.0, 1.0);
    theme::set_source(cr, theme::BRIGHT.alpha(0.18));
    cr.set_line_width(1.5 * s);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    let (fraction, up) = weather::sky_position(now, rise, set);
    let travelled = if up { fraction } else if now >= set { 1.0 } else { 0.0 };
    if travelled > 0.0 {
        trace(cr, 0.0, travelled);
        theme::set_source(cr, SUN_GOLD.alpha(0.85));
        cr.set_line_width(2.0 * s);
        let _ = cr.stroke();
    }
    if up {
        let (x, y) = point(fraction);
        let glow = cairo::RadialGradient::new(x, y, 0.0, x, y, 12.0 * s);
        glow.add_color_stop_rgba(0.0, SUN_GOLD.r, SUN_GOLD.g, SUN_GOLD.b, 0.5);
        glow.add_color_stop_rgba(1.0, SUN_GOLD.r, SUN_GOLD.g, SUN_GOLD.b, 0.0);
        let _ = cr.set_source(&glow);
        cr.arc(x, y, 12.0 * s, 0.0, 2.0 * PI);
        let _ = cr.fill();
        cr.arc(x, y, 4.5 * s, 0.0, 2.0 * PI);
        theme::set_source(cr, SUN_GOLD);
        let _ = cr.fill();
    }
}
