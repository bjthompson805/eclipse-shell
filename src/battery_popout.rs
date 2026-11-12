//! Battery popout, dropping from the battery in the status cluster: the
//! charge and its state, the charge limit, the power profile, and time left,
//! draw, capacity and health.
//!
//! Readings come from `acpiconf -i 0`, rc.conf's powerd settings and the
//! charge-limit file eclipse-priv keeps. Changes go through eclipse-priv (see
//! system/), and the card is rebuilt from fresh readings once each finishes.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, FAINT, FLAME, ICON_DIM, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-battery";
const HELPER: &str = "/usr/local/libexec/eclipse-priv";
const LIMIT_STATE: &str = "/var/db/eclipse-charge-limit";
const TICK: Duration = Duration::from_secs(5);
const LIMIT_STEP: u32 = 5;
const LOW: u32 = 20;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Profile {
    Saver,
    Balanced,
    Performance,
}

impl Profile {
    const ALL: [Profile; 3] = [Profile::Saver, Profile::Balanced, Profile::Performance];

    /// The powerd flags eclipse-priv writes for each profile.
    fn flags(self) -> &'static str {
        match self {
            Profile::Saver => "-a adaptive -b minimum",
            Profile::Balanced => "",
            Profile::Performance => "-a maximum -b hiadaptive",
        }
    }

    /// The profile rc.conf's powerd settings select, if any.
    pub(crate) fn from_rc(powerd_enable: &str, powerd_flags: &str) -> Option<Profile> {
        let flags = powerd_flags.split_whitespace().collect::<Vec<_>>().join(" ");
        powerd_enable.trim().eq_ignore_ascii_case("yes").then(|| Profile::ALL.into_iter().find(|p| p.flags() == flags)).flatten()
    }

    fn arg(self) -> &'static str {
        match self {
            Profile::Saver => "saver",
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Profile::Saver => "battery_saver",
            Profile::Balanced => "balance",
            Profile::Performance => "speed",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Profile::Saver => "Saver",
            Profile::Balanced => "Balanced",
            Profile::Performance => "Perf",
        }
    }
}

#[derive(Clone, Copy)]
struct ChargeLimit {
    enabled: bool,
    end: u32,
    full_when_off: bool,
}

#[derive(Clone, Default)]
struct Reading {
    percent: u32,
    charging: bool,
    discharging: bool,
    on_ac: bool,
    /// Minutes, while known.
    minutes_left: Option<u32>,
    /// Watts flowing in or out.
    watts: Option<f64>,
    full_wh: Option<f64>,
    health: Option<u32>,
    profile: Option<Profile>,
    limit: Option<ChargeLimit>,
    /// eclipse-priv is installed, so the controls can change things.
    can_change: bool,
}

impl Reading {
    fn low(&self) -> bool {
        !self.charging && self.percent <= LOW
    }

    /// Plugged in and sitting at the limit rather than charging past it.
    fn holding(&self) -> bool {
        self.on_ac && !self.charging && self.limit.is_some_and(|l| l.enabled && self.percent + 1 >= l.end && self.percent < 100)
    }

    fn state_label(&self) -> String {
        if self.holding() {
            return format!("Holding at {}%", self.limit.map_or(100, |l| l.end));
        }
        match () {
            _ if self.charging => "Charging".into(),
            _ if self.discharging => "On battery".into(),
            _ if self.on_ac && self.percent >= 99 => "Full".into(),
            _ if self.on_ac => "Plugged in".into(),
            _ => "On battery".into(),
        }
    }

    fn tint(&self) -> Rgba {
        if self.low() {
            theme::palette().accent_lit
        } else if self.charging {
            FLAME
        } else {
            CREAM
        }
    }
}

pub struct BatteryPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
    /// A change is running; the controls ignore clicks until it finishes.
    busy: Rc<Cell<bool>>,
}

impl BatteryPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self { popout: Popout::new(app, NAMESPACE), generation: Cell::new(0), busy: Rc::default() })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let battery = self.clone();
        glib::spawn_future_local(async move {
            battery.refresh().await;
            battery.popout.show(Anchor::Under(x));
        });

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let battery = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match battery.upgrade() {
            Some(b) if b.popout.is_open() && b.generation.get() == generation => {
                glib::spawn_future_local(async move { b.refresh().await });
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    async fn refresh(self: &Rc<Self>) {
        let reading = read().await;
        let s = self.popout.scale();
        self.popout.set_content(&popout::card(&self.body(s, &reading)));
    }

    /// Runs eclipse-priv with `args`, then reads everything again.
    fn change(self: &Rc<Self>, args: Vec<String>) {
        if self.busy.replace(true) {
            return;
        }
        let battery = self.clone();
        glib::spawn_future_local(async move {
            let mut argv = vec!["sudo".to_owned(), "-n".to_owned(), HELPER.to_owned()];
            argv.extend(args);
            let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
            if let Ok(p) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE) {
                if let Ok((_, Some(err))) = p.communicate_utf8_future(None).await
                    && !err.trim().is_empty()
                {
                    eprintln!("eclipse-shell: eclipse-priv: {}", err.trim());
                }
            }
            battery.busy.set(false);
            if battery.popout.is_open() {
                battery.refresh().await;
            }
        });
    }

    fn body(self: &Rc<Self>, s: f64, r: &Reading) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(11.0))
            .width_request(px(260.0) - 2 * px(16.0))
            .margin_top(px(13.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();
        let palette = theme::palette();

        let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        let (glyph, glyph_color) = match () {
            _ if r.low() => ("battery_alert", palette.accent_lit),
            _ if r.charging => ("battery_charging_full", FLAME),
            _ => ("battery_full", palette.accent),
        };
        header.append(&theme::filled_icon_glyph(glyph, 16.0 * s, glyph_color));
        header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("BATTERY"));
        column.append(&header);

        let charge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let percent = TextStyle::new(26.0 * s, pango::Weight::Semibold, r.tint()).tracking(-0.5 * s).label(&format!("{}%", r.percent));
        percent.set_hexpand(true);
        percent.set_xalign(0.0);
        charge.append(&percent);
        let state = TextStyle::new(11.0 * s, pango::Weight::Semibold, if r.charging { FLAME } else { SUBTLE }).label(&r.state_label());
        state.set_valign(gtk::Align::Baseline);
        percent.set_valign(gtk::Align::Baseline);
        charge.append(&state);
        column.append(&charge);
        column.append(&meter(s, f64::from(r.percent) / 100.0, r.tint()));

        if let Some(limit) = r.limit {
            column.append(&popout::divider());
            column.append(&self.limit_section(s, limit, r.can_change));
        }

        column.append(&popout::divider());
        let profile = gtk::Box::new(gtk::Orientation::Vertical, px(7.0));
        profile.append(&popout::micro_label(s, "Profile"));
        let tabs = gtk::Box::builder().spacing(px(3.0)).homogeneous(true).build();
        for option in Profile::ALL {
            let battery = Rc::downgrade(self);
            tabs.append(&profile_tab(s, option, r.profile == Some(option), r.can_change, move || {
                battery.upgrade().into_iter().for_each(|b| b.change(vec!["power-profile".into(), option.arg().into()]))
            }));
        }
        profile.append(&tabs);
        column.append(&profile);

        let mut rows = Vec::new();
        if let Some(minutes) = r.minutes_left {
            rows.push((if r.charging { "Until full" } else { "Remaining" }, format!("{}:{:02}", minutes / 60, minutes % 60)));
        }
        if let Some(w) = r.watts.filter(|w| *w > 0.0) {
            rows.push((if r.charging { "Charging" } else { "Draw" }, format!("{w:.1} W")));
        }
        if let Some(wh) = r.full_wh {
            rows.push(("Capacity", format!("{wh:.1} Wh")));
        }
        if let Some(health) = r.health {
            rows.push(("Health", format!("{health}%")));
        }
        if !rows.is_empty() {
            column.append(&popout::divider());
            let details = gtk::Box::new(gtk::Orientation::Vertical, px(8.0));
            for (label, value) in rows {
                details.append(&info_row(s, label, &value));
            }
            column.append(&details);
        }
        if !r.can_change {
            let note = TextStyle::new(9.0 * s, pango::Weight::Normal, FAINT).label("Run install.sh to change these.");
            note.set_xalign(0.0);
            note.set_wrap(true);
            column.append(&note);
        }
        column
    }

    fn limit_section(self: &Rc<Self>, s: f64, limit: ChargeLimit, can_change: bool) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let section = gtk::Box::new(gtk::Orientation::Vertical, px(7.0));
        let battery = Rc::downgrade(self);
        let run = move |args: Vec<&str>| {
            let args = args.into_iter().map(String::from).collect();
            if let Some(b) = battery.upgrade() {
                b.change(args);
            }
        };

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let label = popout::micro_label(s, "Charge limit");
        label.set_hexpand(true);
        head.append(&label);
        head.append(&popout::switch(s, limit.enabled, can_change, {
            let run = run.clone();
            move || run(vec!["charge-limit", if limit.enabled { "off" } else { "on" }])
        }));
        section.append(&head);

        // "Stop at" with a stepper.
        let stop = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
        let label = TextStyle::new(11.0 * s, pango::Weight::Medium, CREAM).label("Stop at");
        label.set_hexpand(true);
        label.set_xalign(0.0);
        stop.append(&label);
        let lowered = limit.end.saturating_sub(LIMIT_STEP).max(LIMIT_STEP);
        let raised = (limit.end + LIMIT_STEP).min(100);
        stop.append(&step_button(s, "remove", can_change && limit.end > LIMIT_STEP, {
            let run = run.clone();
            move || run(vec!["charge-limit", "set", &lowered.to_string()])
        }));
        let value = TextStyle::new(12.0 * s, pango::Weight::Semibold, CREAM).label(&format!("{}%", limit.end));
        value.set_width_chars(4);
        stop.append(&value);
        stop.append(&step_button(s, "add", can_change && limit.end < 100, {
            let run = run.clone();
            move || run(vec!["charge-limit", "set", &raised.to_string()])
        }));
        section.append(&stop);

        let full = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        let label = TextStyle::new(11.0 * s, pango::Weight::Medium, CREAM).label("Charge to full when off");
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.set_ellipsize(pango::EllipsizeMode::End);
        full.append(&label);
        full.append(&popout::switch(s, limit.full_when_off, can_change, move || {
            run(vec!["charge-limit", "full-when-off", if limit.full_when_off { "off" } else { "on" }])
        }));
        section.append(&full);
        section
    }
}

// ---- readings ----------------------------------------------------------------

async fn read() -> Reading {
    let acpi = output(&["acpiconf", "-i", "0"]).await.unwrap_or_default();
    let fields: HashMap<&str, &str> = acpi.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect();
    let number = |key: &str| fields.get(key).and_then(|v| v.split_whitespace().next()).and_then(|v| v.trim_end_matches('%').parse::<f64>().ok());
    let state = fields.get("State").copied().unwrap_or("");
    let on_ac = output(&["sysctl", "-n", "hw.acpi.acline"]).await.is_some_and(|v| v.trim() == "1");
    // "2:47", or "unknown" while the firmware can't tell.
    let minutes_left = fields.get("Remaining time").and_then(|t| {
        let (h, m) = t.split_once(':')?;
        Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
    });
    let (full, design) = (number("Last full capacity"), number("Design capacity"));

    let rc = output(&["sh", "-c", "sysrc -n powerd_enable 2>/dev/null; sysrc -n powerd_flags 2>/dev/null"]).await.unwrap_or_default();
    let mut lines = rc.lines();
    let profile = Profile::from_rc(lines.next().unwrap_or(""), lines.next().unwrap_or(""));

    Reading {
        percent: number("Remaining capacity").unwrap_or(0.0).clamp(0.0, 100.0) as u32,
        charging: state.contains("charging") && !state.contains("discharging"),
        discharging: state.contains("discharging"),
        on_ac,
        minutes_left,
        watts: number("Present rate").map(|mw| mw / 1000.0),
        full_wh: full.map(|mwh| mwh / 1000.0),
        health: full.zip(design).filter(|(_, d)| *d > 0.0).map(|(f, d)| (100.0 * f / d).round().min(100.0) as u32),
        profile,
        limit: read_limit(),
        can_change: std::path::Path::new(HELPER).exists(),
    }
}

/// The charge limit, when eclipse-priv knows the battery supports one.
fn read_limit() -> Option<ChargeLimit> {
    let text = std::fs::read_to_string(LIMIT_STATE).ok()?;
    let settings: HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once(' ')).collect();
    if settings.get("supported") != Some(&"true") {
        return None;
    }
    Some(ChargeLimit {
        enabled: settings.get("enabled") == Some(&"true"),
        end: settings.get("end").and_then(|e| e.parse().ok()).unwrap_or(80),
        full_when_off: settings.get("full_when_off") == Some(&"true"),
    })
}

async fn output(argv: &[&str]) -> Option<String> {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    stdout.map(String::from)
}

// ---- pieces ------------------------------------------------------------------


fn info_row(s: f64, label: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let name = popout::micro_label(s, label);
    name.set_hexpand(true);
    name.set_valign(gtk::Align::Baseline);
    row.append(&name);
    let value = TextStyle::new(11.0 * s, pango::Weight::Semibold, CREAM).label(value);
    value.set_valign(gtk::Align::Baseline);
    row.append(&value);
    row
}


fn meter(s: f64, fraction: f64, fill: Rgba) -> gtk::DrawingArea {
    let bar = gtk::DrawingArea::builder().content_height((6.0 * s).round() as i32).hexpand(true).build();
    bar.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
        theme::set_source(cr, theme::palette().raised);
        let _ = cr.fill();
        let filled = (w * fraction.clamp(0.0, 1.0)).round();
        if filled > 0.0 {
            theme::rounded_rect(cr, 0.0, 0.0, filled.max(h), h, h / 2.0);
            theme::set_source(cr, fill);
            let _ = cr.fill();
        }
    });
    bar
}



/// A small square button holding a glyph, outlined on hover.
fn step_button(s: f64, glyph: &'static str, enabled: bool, pressed: impl Fn() + 'static) -> gtk::DrawingArea {
    let px = (20.0 * s).round() as i32;
    let area = gtk::DrawingArea::builder().content_width(px).content_height(px).valign(gtk::Align::Center).build();
    let hovered = Rc::new(Cell::new(false));
    area.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            let palette = theme::palette();
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 6.0 * s);
            theme::set_source(cr, if hovered.get() { palette.raised } else { BRIGHT.alpha(0.04) });
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, if hovered.get() { palette.edge } else { palette.outline });
            let _ = cr.stroke();
            let color = if !enabled { FAINT } else if hovered.get() { CREAM } else { ICON_DIM };
            draw_glyph(cr, glyph, 14.0 * s, color, w, h);
        }
    });
    popout::clickable(&area, &hovered, enabled, pressed);
    area
}

fn profile_tab(s: f64, profile: Profile, current: bool, enabled: bool, pressed: impl Fn() + 'static) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_height((26.0 * s).round() as i32).hexpand(true).build();
    let hovered = Rc::new(Cell::new(false));
    area.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            let palette = theme::palette();
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 10.0);
            if current || hovered.get() {
                theme::set_source(cr, if current { palette.raised } else { BRIGHT.alpha(0.04) });
                let _ = cr.fill_preserve();
            }
            cr.set_line_width(1.0);
            theme::set_source(cr, if current { palette.accent_lit } else { palette.outline });
            let _ = cr.stroke();

            // Glyph and label, centred together.
            let icon = layout(cr, profile.glyph(), TextStyle { family: crate::config::get().icon_font.as_str(), ..TextStyle::new(13.0 * s, pango::Weight::Medium, if current { palette.accent_lit } else { ICON_DIM }) });
            let text = layout(cr, profile.label(), TextStyle::new(9.5 * s, pango::Weight::Semibold, if current { CREAM } else { SUBTLE }));
            let (iw, ih) = size(&icon);
            let (tw, th) = size(&text);
            let gap = 5.0 * s;
            let x = (w - iw - gap - tw) / 2.0;
            cr.move_to(x, (h - ih) / 2.0);
            pangocairo::functions::show_layout(cr, &icon);
            cr.move_to(x + iw + gap, (h - th) / 2.0);
            pangocairo::functions::show_layout(cr, &text);
        }
    });
    popout::clickable(&area, &hovered, enabled && !current, pressed);
    area
}

fn layout(cr: &cairo::Context, text: &str, style: TextStyle) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text(text);
    layout.set_attributes(Some(&style.attrs()));
    layout
}

fn size(layout: &pango::Layout) -> (f64, f64) {
    let (_, r) = layout.pixel_extents();
    (f64::from(r.width()), f64::from(r.height()))
}

fn draw_glyph(cr: &cairo::Context, name: &str, px: f64, color: Rgba, w: f64, h: f64) {
    let glyph = layout(cr, name, TextStyle { family: crate::config::get().icon_font.as_str(), ..TextStyle::new(px, pango::Weight::Medium, color) });
    let (gw, gh) = size(&glyph);
    cr.move_to((w - gw) / 2.0, (h - gh) / 2.0);
    pangocairo::functions::show_layout(cr, &glyph);
}
