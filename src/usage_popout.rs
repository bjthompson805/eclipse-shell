//! Claude Code usage popout, dropping from the bar's usage readout: the
//! plan, both rate-limit windows with when they reset, and the last week's
//! tokens by day and by model. Rebuilt whenever the numbers change, and
//! every few seconds while open so "ago" times stay current.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};
use gtk4 as gtk;

use crate::agent_usage::{self, format_tokens, Limit, Usage};
use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, DIM, HAIR, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-usage";
const WARN_ABOVE: u32 = 80;
const TICK: Duration = Duration::from_secs(5);

pub struct UsagePopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
}

impl UsagePopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let usage = Rc::new(Self { popout: Popout::new(app, NAMESPACE), generation: Cell::new(0) });
        agent_usage::on_change({
            let usage = Rc::downgrade(&usage);
            move || usage.upgrade().into_iter().filter(|u| u.popout.is_open()).for_each(|u| u.rebuild())
        });
        usage
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        self.rebuild();
        self.popout.show(Anchor::Under(x));

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let usage = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match usage.upgrade() {
            Some(u) if u.popout.is_open() && u.generation.get() == generation => {
                u.rebuild();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    fn rebuild(&self) {
        let s = self.popout.scale();
        self.popout.set_content(&popout::card(&body(s, &agent_usage::current())));
    }
}

fn body(s: f64, usage: &Usage) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(px(14.0))
        .width_request(px(260.0) - 2 * px(16.0))
        .margin_top(px(13.0))
        .margin_bottom(px(14.0))
        .margin_start(px(16.0))
        .margin_end(px(16.0))
        .build();
    let now = glib::real_time() / 1_000_000;

    // Header: sparkle, provider, plan, and a refresh button.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    header.append(&theme::filled_icon_glyph("auto_awesome", 16.0 * s, theme::palette().accent));
    header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("CLAUDE CODE"));
    let plan = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(&usage.plan);
    plan.set_hexpand(true);
    plan.set_xalign(0.0);
    header.append(&plan);
    header.append(&refresh_button(s, usage.refreshing));
    column.append(&header);

    if let Some(staleness) = staleness(usage, now) {
        let label = small(s, DIM).label(&staleness);
        label.set_xalign(1.0);
        column.append(&label);
    }

    let have_limits = usage.session.is_some() || usage.weekly.is_some();
    if have_limits {
        let limits = section(s, "Limits", 12.0);
        for (name, limit) in [("Session (5-hour)", usage.session), ("Weekly (7-day)", usage.weekly)] {
            if let Some(limit) = limit {
                limits.append(&limit_row(s, name, limit, now));
            }
        }
        column.append(&limits);
    }
    if let Some(problem) = &usage.problem {
        let color = if have_limits { theme::palette().accent_lit } else { DIM };
        let label = TextStyle::new(10.5 * s, pango::Weight::Normal, color).label(problem);
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.set_max_width_chars(1);
        label.set_hexpand(true);
        column.append(&label);
    }

    if !usage.days.is_empty() {
        column.append(&divider());
        let days = section(s, "Tokens by day", 8.0);
        let peak = usage.days.iter().map(|(_, t)| *t).max().unwrap_or(1).max(1);
        let today = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y-%m-%d").ok()).map(String::from).unwrap_or_default();
        for (day, tokens) in &usage.days {
            days.append(&day_row(s, day, *tokens, peak, *day == today));
        }
        column.append(&days);
    }

    if !usage.models.is_empty() {
        column.append(&divider());
        let models = section(s, "Tokens by model", 6.0);
        let top = usage.models[0].1.max(1);
        for (name, tokens) in &usage.models {
            models.append(&model_row(s, name, *tokens, *tokens as f64 / top as f64));
        }
        column.append(&models);
    }
    column
}

fn small(s: f64, color: Rgba) -> TextStyle {
    TextStyle::new(9.0 * s, pango::Weight::Normal, color)
}

/// A titled group: spaced caps over its rows.
fn section(s: f64, title: &str, spacing: f64) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, (spacing * s).round() as i32);
    let header = TextStyle::new(9.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.4 * s).label(&title.to_uppercase());
    header.set_xalign(0.0);
    group.append(&header);
    group
}

fn divider() -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_height(1).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, HAIR);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line
}

/// A thin rounded track filled to `fraction`.
fn meter(s: f64, fraction: f64, fill: Rgba) -> gtk::DrawingArea {
    let bar = gtk::DrawingArea::builder().content_height((3.0 * s).round().max(1.0) as i32).hexpand(true).valign(gtk::Align::Center).build();
    bar.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        let pill = |cr: &cairo::Context, width: f64| theme::rounded_rect(cr, 0.0, 0.0, width, h, h / 2.0);
        pill(cr, w);
        theme::set_source(cr, BRIGHT.alpha(0.08));
        let _ = cr.fill();
        let filled = w * fraction.clamp(0.0, 1.0);
        if filled > 0.0 {
            pill(cr, filled.max(h));
            theme::set_source(cr, fill);
            let _ = cr.fill();
        }
    });
    bar
}

fn limit_row(s: f64, name: &str, limit: Limit, now: i64) -> gtk::Box {
    let warn = limit.percent > WARN_ABOVE;
    let palette = theme::palette();
    let row = gtk::Box::new(gtk::Orientation::Vertical, (5.0 * s).round() as i32);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let label = TextStyle::new(11.0 * s, pango::Weight::Medium, SUBTLE).label(name);
    label.set_hexpand(true);
    label.set_xalign(0.0);
    top.append(&label);
    top.append(&TextStyle::new(13.0 * s, pango::Weight::Semibold, if warn { palette.accent_lit } else { CREAM }).label(&format!("{}%", limit.percent)));
    row.append(&top);
    row.append(&meter(s, f64::from(limit.percent) / 100.0, if warn { palette.accent_lit } else { palette.accent }));
    if let Some(at) = limit.resets_at {
        let resets = small(s, DIM).label(&format!("Resets in {}", duration(at - now)));
        resets.set_xalign(0.0);
        row.append(&resets);
    }
    row
}

fn day_row(s: f64, day: &str, tokens: u64, peak: u64, today: bool) -> gtk::Box {
    let palette = theme::palette();
    let row = gtk::Box::builder().spacing((8.0 * s).round() as i32).margin_top((2.0 * s).round() as i32).margin_bottom((2.0 * s).round() as i32).build();
    let style = TextStyle::new(9.5 * s, if today { pango::Weight::Semibold } else { pango::Weight::Medium }, if today { CREAM } else { DIM });
    let label = style.label(&day_name(day, today));
    label.set_xalign(0.0);
    label.set_size_request((30.0 * s).round() as i32, -1);
    row.append(&label);
    row.append(&meter(s, tokens as f64 / peak as f64, if today { palette.accent } else { palette.accent.alpha(0.55) }));
    // A fixed-width count column keeps every bar the same length.
    let count = TextStyle::new(9.5 * s, pango::Weight::Normal, SUBTLE).label(&format_tokens(tokens));
    count.set_width_chars(6);
    count.set_xalign(1.0);
    row.append(&count);
    row
}

/// A model's name and tokens over a soft bar showing its share of the top
/// model's total.
fn model_row(s: f64, name: &str, tokens: u64, share: f64) -> gtk::Overlay {
    let radius = 6.0 * s;
    let background = gtk::DrawingArea::new();
    background.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
        theme::set_source(cr, BRIGHT.alpha(0.05));
        let _ = cr.fill();
        theme::rounded_rect(cr, 0.0, 0.0, (w * share.clamp(0.0, 1.0)).max(2.0 * radius), h, radius);
        theme::set_source(cr, theme::palette().accent.alpha(0.16));
        let _ = cr.fill();
    });
    let text = gtk::Box::builder()
        .spacing((8.0 * s).round() as i32)
        .margin_start((8.0 * s).round() as i32)
        .margin_end((8.0 * s).round() as i32)
        .margin_top((6.0 * s).round() as i32)
        .margin_bottom((6.0 * s).round() as i32)
        .build();
    let label = TextStyle::new(10.5 * s, pango::Weight::Medium, CREAM).label(name);
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    text.append(&label);
    text.append(&TextStyle::new(10.5 * s, pango::Weight::Normal, DIM).label(&format_tokens(tokens)));
    let row = gtk::Overlay::new();
    row.set_child(Some(&background));
    row.add_overlay(&text);
    row.set_measure_overlay(&text, true);
    row
}

/// A round arrow that checks again now; dimmed while a check is running.
fn refresh_button(s: f64, busy: bool) -> gtk::Label {
    let color = if busy { DIM } else { SUBTLE };
    let button = theme::icon_glyph("refresh", 16.0 * s, color);
    if busy {
        return button;
    }
    button.set_cursor_from_name(Some("pointer"));
    let click = gtk::GestureClick::new();
    click.connect_released(|_, _, _, _| agent_usage::refresh_now());
    button.add_controller(click);
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter(move |m, _, _| {
        if let Some(label) = m.widget().and_downcast::<gtk::Label>() {
            label.set_attributes(theme::icon_glyph("refresh", 16.0 * s, CREAM).attributes().as_ref());
        }
    });
    motion.connect_leave(move |m| {
        if let Some(label) = m.widget().and_downcast::<gtk::Label>() {
            label.set_attributes(theme::icon_glyph("refresh", 16.0 * s, color).attributes().as_ref());
        }
    });
    button.add_controller(motion);
    button
}

/// "Updated 2m ago", or with when it last tried too after a failed check.
fn staleness(usage: &Usage, now: i64) -> Option<String> {
    let updated = usage.updated_at.map(|t| format!("Updated {}", ago(now - t)));
    match (updated, usage.problem.is_some().then_some(usage.checked_at).flatten()) {
        (Some(updated), Some(checked)) => Some(format!("{updated} \u{b7} retried {}", ago(now - checked))),
        (None, Some(checked)) => Some(format!("Checked {}", ago(now - checked))),
        (updated, None) => updated,
    }
}

fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 5 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s => format!("{} ago", duration(s)),
    }
}

/// "2d 4h", "3h 12m" or "8m".
fn duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".into();
    }
    let minutes = seconds / 60;
    let (days, hours) = (minutes / 1440, minutes / 60);
    if days > 0 {
        format!("{days}d {}h", hours % 24)
    } else if hours > 0 {
        format!("{hours}h {}m", minutes % 60)
    } else {
        format!("{}m", minutes.max(1))
    }
}

/// "Today", or the weekday's short name.
fn day_name(day: &str, today: bool) -> String {
    if today {
        return "Today".into();
    }
    let mut parts = day.split('-').map(|p| p.parse::<i32>().ok());
    let (Some(Some(y)), Some(Some(m)), Some(Some(d))) = (parts.next(), parts.next(), parts.next()) else { return day.into() };
    glib::DateTime::from_local(y, m, d, 0, 0, 0.0).ok().and_then(|d| d.format("%a").ok()).map_or_else(|| day.into(), String::from)
}
