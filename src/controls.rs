//! The right sidebar: quick controls over a tabbed glance. A clock and the
//! weather, quick toggles (Wi-Fi, Bluetooth, do not disturb, night light,
//! microphone, airplane mode), volume and brightness faders, then a tab rail
//! over notifications, the calendar, now playing and the forecast.
//!
//! The tab panes are the popouts' own cards, each popout built a second time
//! on a [`Popout::pane`] here, so they stay identical and keep their own live
//! updates. Only the chosen pane is open, and only while the sidebar is.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use gtk4 as gtk;

use crate::calendar::Calendar;
use crate::display_popout::{read_brightness, set_brightness, MIN_BRIGHTNESS};
use crate::fader::{self, Fader};
use crate::inbox_popout::InboxPopout;
use crate::media_popout::MediaPopout;
use crate::popout::{self, Popout};
use crate::sidebar::{Side, Sidebar};
use crate::theme::{self, TextStyle, BRIGHT, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};
use crate::weather_popout::WeatherPopout;
use crate::{audio, bar, bluetooth, notifications, weather, wifi};

/// Wide enough for the popout cards in the panes.
const WIDTH: f64 = 410.0;
/// How often the toggles and faders re-read their state while open.
const POLL: Duration = Duration::from_secs(2);
/// A toggle's change shows up in its state after about this long.
const SETTLE: Duration = Duration::from_millis(700);
const HELPER: &str = "/usr/local/libexec/eclipse-priv";
const NIGHT_TEMPERATURE: &str = "4500";

/// What the toggles show. `None`: there's no such radio.
#[derive(Clone, Default, PartialEq)]
struct State {
    wifi: Option<bool>,
    bluetooth: Option<bool>,
    night: bool,
    mic_muted: bool,
    volume: Option<(f64, bool)>,
}

struct Pane {
    glyph: &'static str,
    name: &'static str,
    popout: Rc<Popout>,
    /// Opens the pane's content (the popout's toggle, while it's shut).
    open: Box<dyn Fn()>,
}

pub struct Controls {
    sidebar: Rc<Sidebar>,
    panes: Vec<Pane>,
    chosen: Cell<usize>,
    open: Cell<bool>,
    generation: Cell<u32>,
    state: RefCell<State>,
    toggles: RefCell<Option<gtk::Box>>,
    volume: RefCell<Option<Rc<Fader>>>,
    brightness: RefCell<Option<Rc<Fader>>>,
    rail: RefCell<Option<gtk::Box>>,
    /// The header's weather, refilled as readings arrive.
    weather: RefCell<Option<gtk::Box>>,
}

impl Controls {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let sidebar = Sidebar::new(app, Side::Right, "eclipse-shell-controls", WIDTH);
        let holder = || gtk::Box::new(gtk::Orientation::Vertical, 0);
        let (h_inbox, h_calendar, h_media, h_weather) = (holder(), holder(), holder(), holder());
        let pane_popout = |h: &gtk::Box| Popout::pane(app, h, sidebar.hints(), 1.0);
        let (p_inbox, p_calendar, p_media, p_weather) = (pane_popout(&h_inbox), pane_popout(&h_calendar), pane_popout(&h_media), pane_popout(&h_weather));
        let inbox = InboxPopout::with_popout(p_inbox.clone());
        let calendar = Calendar::with_popout(p_calendar.clone());
        let media = MediaPopout::with_popout(p_media.clone());
        let weather = WeatherPopout::with_popout(p_weather.clone());
        let panes = vec![
            Pane { glyph: "notifications", name: "Inbox", popout: p_inbox, open: Box::new(move || inbox.toggle(0.0)) },
            Pane { glyph: "calendar_month", name: "Calendar", popout: p_calendar, open: Box::new(move || calendar.toggle(0.0)) },
            Pane { glyph: "music_note", name: "Media", popout: p_media, open: Box::new(move || media.toggle(0.0)) },
            Pane { glyph: "partly_cloudy_day", name: "Weather", popout: p_weather, open: Box::new(move || weather.toggle(0.0)) },
        ];

        let controls = Rc::new(Self {
            sidebar,
            panes,
            chosen: Cell::new(0),
            open: Cell::new(false),
            generation: Cell::new(0),
            state: RefCell::default(),
            toggles: RefCell::default(),
            volume: RefCell::default(),
            brightness: RefCell::default(),
            rail: RefCell::default(),
            weather: RefCell::default(),
        });
        let holders = [h_inbox, h_calendar, h_media, h_weather];
        controls.sidebar.connect_open({
            let controls = Rc::downgrade(&controls);
            move |open, s| {
                let Some(c) = controls.upgrade() else { return };
                c.open.set(open);
                c.generation.set(c.generation.get().wrapping_add(1));
                if open {
                    c.build(s, &holders);
                } else {
                    c.panes.iter().filter(|p| p.popout.is_open()).for_each(|p| p.popout.hide());
                    *c.volume.borrow_mut() = None;
                    *c.brightness.borrow_mut() = None;
                }
            }
        });
        notifications::on_change({
            let controls = Rc::downgrade(&controls);
            move |_| {
                if let Some(c) = controls.upgrade().filter(|c| c.open.get()) {
                    c.show_toggles();
                }
            }
        });
        weather::on_change({
            let controls = Rc::downgrade(&controls);
            move || {
                if let Some(c) = controls.upgrade().filter(|c| c.open.get()) {
                    c.show_weather();
                }
            }
        });
        controls
    }

    pub fn sidebar(&self) -> &Rc<Sidebar> {
        &self.sidebar
    }

    fn build(self: &Rc<Self>, s: f64, holders: &[gtk::Box; 4]) {
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::new(gtk::Orientation::Vertical, px(16.0));
        column.set_margin_start(px(16.0));
        column.set_margin_end(px(12.0));
        column.set_margin_top(px(16.0));
        column.set_margin_bottom(px(18.0));

        let (head, weather) = header(s);
        column.append(&head);
        *self.weather.borrow_mut() = Some(weather);
        self.show_weather();

        let toggles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&toggles);
        *self.toggles.borrow_mut() = Some(toggles);
        self.show_toggles();

        // Faders: volume (glyph click mutes) and the backlight, filled in by the first poll.
        let faders = gtk::Box::new(gtk::Orientation::Vertical, px(6.0));
        let options = move |glyphs, floor, send_every| fader::Options { glyphs, glyph_px: 17.0 * s, inline: true, floor, send_every: Duration::from_millis(send_every) };
        let state = self.state.borrow().clone();
        let (volume, muted) = state.volume.unwrap_or((0.0, false));
        let on_mute: Box<dyn Fn(bool)> = Box::new(|muted| audio::spawn(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", if muted { "1" } else { "0" }]));
        let volume = Fader::new(s, volume, muted, options(("volume_up", "volume_off"), 0.0, 40), |v| audio::spawn(&["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{v:.3}")]), Some(on_mute));
        faders.append(&volume.widget);
        *self.volume.borrow_mut() = Some(volume);
        column.append(&faders);
        let generation = self.generation.get();
        let controls = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(value) = read_brightness().await else { return };
            let Some(c) = controls.upgrade().filter(|c| c.generation.get() == generation) else { return };
            let fader = Fader::new(s, value, false, options(("light_mode", "light_mode"), MIN_BRIGHTNESS, 40), set_brightness, None);
            faders.append(&fader.widget);
            *c.brightness.borrow_mut() = Some(fader);
        });

        column.append(&popout::divider());
        let rail = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        rail.set_homogeneous(true);
        column.append(&rail);
        *self.rail.borrow_mut() = Some(rail);
        self.show_rail(s);
        let panes = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for holder in holders {
            // The cards keep their own widths; centre them in the column.
            holder.set_halign(gtk::Align::Center);
            // Still in the last open's column.
            if let Some(old) = holder.parent().and_downcast::<gtk::Box>() {
                old.remove(holder);
            }
            panes.append(holder);
        }
        column.append(&panes);

        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).margin_end(px(6.0)).build();
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&scroller);
        self.sidebar.set_content(&outer);

        self.panes.iter().for_each(|p| p.popout.set_pane_scale(s));
        self.open_pane(self.chosen.get());
        self.poll();
    }

    fn show_weather(&self) {
        let Some(side) = self.weather.borrow().clone() else { return };
        let s = self.sidebar.scale();
        let px = |v: f64| (v * s).round() as i32;
        while let Some(child) = side.first_child() {
            side.remove(&child);
        }
        let Some(now) = weather::current() else { return };
        let top = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
        top.set_halign(gtk::Align::End);
        top.append(&theme::filled_icon_glyph(now.glyph.symbol(), 22.0 * s, theme::palette().accent_lit));
        top.append(&TextStyle::new(18.0 * s, pango::Weight::Semibold, CREAM).label(&now.temp));
        side.append(&top);
        let condition = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).label(now.condition);
        condition.set_xalign(1.0);
        side.append(&condition);
    }

    fn open_pane(&self, index: usize) {
        for (i, pane) in self.panes.iter().enumerate() {
            if i != index && pane.popout.is_open() {
                pane.popout.hide();
            }
        }
        self.chosen.set(index);
        if let Some(pane) = self.panes.get(index).filter(|p| !p.popout.is_open()) {
            (pane.open)();
        }
    }

    fn show_rail(self: &Rc<Self>, s: f64) {
        let Some(rail) = self.rail.borrow().clone() else { return };
        while let Some(child) = rail.first_child() {
            rail.remove(&child);
        }
        for (i, pane) in self.panes.iter().enumerate() {
            let controls = Rc::downgrade(self);
            rail.append(&tab(s, pane.glyph, pane.name, i == self.chosen.get(), move || {
                if let Some(c) = controls.upgrade() {
                    c.open_pane(i);
                    c.show_rail(s);
                }
            }));
        }
    }

    /// Re-reads the toggles' state every few seconds while open.
    fn poll(self: &Rc<Self>) {
        let generation = self.generation.get();
        let controls = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            loop {
                let state = gio::spawn_blocking(read_state).await.unwrap_or_default();
                let Some(c) = controls.upgrade().filter(|c| c.open.get() && c.generation.get() == generation) else { return };
                if let (Some(fader), Some((v, muted))) = (c.volume.borrow().as_ref(), state.volume) {
                    fader.show(v, muted);
                }
                if *c.state.borrow() != state {
                    *c.state.borrow_mut() = state;
                    c.show_toggles();
                }
                if let Some(value) = read_brightness().await {
                    if let Some(fader) = c.brightness.borrow().as_ref() {
                        fader.show(value, false);
                    }
                }
                drop(c);
                glib::timeout_future(POLL).await;
            }
        });
    }

    /// Re-reads soon after a toggle, once the change has landed.
    fn settle(self: &Rc<Self>) {
        let controls = Rc::downgrade(self);
        glib::timeout_add_local_once(SETTLE, move || {
            let Some(c) = controls.upgrade() else { return };
            glib::spawn_future_local(async move {
                let state = gio::spawn_blocking(read_state).await.unwrap_or_default();
                if c.open.get() && *c.state.borrow() != state {
                    *c.state.borrow_mut() = state;
                    c.show_toggles();
                }
            });
        });
    }

    fn show_toggles(self: &Rc<Self>) {
        let Some(holder) = self.toggles.borrow().clone() else { return };
        let s = self.sidebar.scale();
        let px = |v: f64| (v * s).round() as i32;
        while let Some(child) = holder.first_child() {
            holder.remove(&child);
        }
        let state = self.state.borrow().clone();
        let grid = gtk::Grid::builder().column_spacing(px(8.0)).row_spacing(px(8.0)).column_homogeneous(true).build();
        let mut tiles: Vec<gtk::Overlay> = Vec::new();
        let weak = Rc::downgrade(self);
        let act = move |f: Box<dyn Fn()>| {
            let weak = weak.clone();
            move || {
                f();
                if let Some(c) = weak.upgrade() {
                    c.settle();
                }
            }
        };
        if let Some(on) = state.wifi {
            tiles.push(tile(s, if on { "wifi" } else { "wifi_off" }, "Wi-Fi", on, act(Box::new(move || set_wifi(!on)))));
        }
        if let Some(on) = state.bluetooth {
            tiles.push(tile(s, if on { "bluetooth" } else { "bluetooth_disabled" }, "Bluetooth", on, act(Box::new(move || set_bluetooth(!on)))));
        }
        let dnd = notifications::dnd();
        tiles.push(tile(s, if dnd { "do_not_disturb_on" } else { "do_not_disturb_off" }, "Do not disturb", dnd, act(Box::new(move || notifications::set_dnd(!dnd)))));
        let night = state.night;
        tiles.push(tile(s, "nightlight", "Night light", night, act(Box::new(move || set_night(!night)))));
        let muted = state.mic_muted;
        tiles.push(tile(s, if muted { "mic_off" } else { "mic" }, if muted { "Mic muted" } else { "Microphone" }, !muted, act(Box::new(|| audio::spawn(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])))));
        if state.wifi.is_some() || state.bluetooth.is_some() {
            // On while no radio is.
            let airplane = state.wifi != Some(true) && state.bluetooth != Some(true);
            let (wifi, bt) = (state.wifi.is_some(), state.bluetooth.is_some());
            tiles.push(tile(s, "flight", "Airplane", airplane, act(Box::new(move || {
                if wifi {
                    set_wifi(airplane);
                }
                if bt {
                    set_bluetooth(airplane);
                }
            }))));
        }
        for (i, t) in tiles.iter().enumerate() {
            grid.attach(t, (i % 3) as i32, (i / 3) as i32, 1, 1);
        }
        holder.append(&grid);
    }
}

/// The clock and date, and a box at the right for the weather.
fn header(s: f64) -> (gtk::Box, gtk::Box) {
    let px = |v: f64| (v * s).round() as i32;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
    let times = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    times.set_hexpand(true);
    let time = TextStyle::new(28.0 * s, pango::Weight::Bold, BRIGHT).tracking(-0.6 * s).label("");
    time.set_xalign(0.0);
    let date = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label("");
    date.set_xalign(0.0);
    times.append(&time);
    times.append(&date);
    row.append(&times);
    let paint = move |now: &glib::DateTime| {
        let format = if crate::config::get().clock_24h { "%H:%M" } else { "%-I:%M %p" };
        time.set_text(&now.format(format).map(String::from).unwrap_or_default());
        date.set_text(&now.format("%A, %-d %B").map(String::from).unwrap_or_default());
    };
    if let Ok(now) = glib::DateTime::now_local() {
        paint(&now);
    }
    bar::every_second(&row, paint);

    let side = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    side.set_valign(gtk::Align::Center);
    row.append(&side);
    (row, side)
}

/// A quick toggle: a glyph over a label, tinted with the accent while on.
fn tile(s: f64, glyph: &str, name: &str, on: bool, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let hovered = Rc::new(Cell::new(false));
    let back = gtk::DrawingArea::new();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 10.0 * s);
            let fill = if on { palette.accent.alpha(0.16) } else { palette.raised };
            theme::set_source(cr, fill);
            let _ = cr.fill_preserve();
            if hovered.get() {
                theme::set_source(cr, theme::ROW_HOVER);
                let _ = cr.fill_preserve();
            }
            cr.set_line_width(1.0);
            let edge = if on { palette.accent.alpha(0.7) } else if hovered.get() { theme::CHIP_EDGE } else { palette.outline };
            theme::set_source(cr, edge);
            let _ = cr.stroke();
        }
    });
    let content = gtk::Box::new(gtk::Orientation::Vertical, px(5.0));
    content.set_margin_top(px(11.0));
    content.set_margin_bottom(px(10.0));
    content.set_margin_start(px(6.0));
    content.set_margin_end(px(6.0));
    let icon = if on { theme::filled_icon_glyph(glyph, 19.0 * s, theme::palette().accent_lit) } else { theme::icon_glyph(glyph, 19.0 * s, ICON_DIM) };
    content.append(&icon);
    let label = TextStyle::new(9.5 * s, pango::Weight::Medium, if on { CREAM } else { SUBTLE }).label(name);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    content.append(&label);
    let tile = gtk::Overlay::new();
    tile.set_child(Some(&back));
    tile.add_overlay(&content);
    tile.set_measure_overlay(&content, true);
    popout::on_hover(&tile, {
        let back = back.downgrade();
        move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
        }
    });
    popout::on_click(&tile, pressed);
    tile
}

/// A tab on the rail: glyph and name, an accent underline when chosen.
fn tab(s: f64, glyph: &str, name: &str, chosen: bool, pressed: impl Fn() + 'static) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let column = gtk::Box::new(gtk::Orientation::Vertical, px(4.0));
    let color = if chosen { theme::palette().accent_lit } else { ICON_DIM };
    let icon = if chosen { theme::filled_icon_glyph(glyph, 18.0 * s, color) } else { theme::icon_glyph(glyph, 18.0 * s, color) };
    column.append(&icon);
    column.append(&TextStyle::new(8.5 * s, pango::Weight::Semibold, if chosen { CREAM } else { FAINT }).mono().tracking(1.0 * s).label(&name.to_uppercase()));
    let line = gtk::DrawingArea::builder().content_height(px(2.0)).margin_start(px(14.0)).margin_end(px(14.0)).margin_top(px(3.0)).build();
    line.set_draw_func(move |_, cr, w, h| {
        if chosen {
            theme::rounded_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), f64::from(h) / 2.0);
            theme::set_source(cr, theme::palette().accent);
            let _ = cr.fill();
        }
    });
    column.append(&line);
    if !chosen {
        popout::on_click(&column, pressed);
    }
    column
}

// ── State and actions ───────────────────────────────────────────────────────

/// Runs on a worker thread.
fn read_state() -> State {
    let wifi = wifi::read();
    let bt = bluetooth::read();
    let volume = |target: &str| {
        let out = Command::new("wpctl").args(["get-volume", target]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        Some((text.split_whitespace().nth(1)?.parse::<f64>().ok()?, text.contains("[MUTED]")))
    };
    State {
        wifi: wifi.iface.is_some().then_some(wifi.radio),
        bluetooth: bt.present.then_some(bt.enabled),
        night: Command::new("pgrep").args(["-x", "hyprsunset"]).output().is_ok_and(|o| o.status.success()),
        mic_muted: volume("@DEFAULT_AUDIO_SOURCE@").is_some_and(|(_, m)| m),
        volume: volume("@DEFAULT_AUDIO_SINK@"),
    }
}

fn set_wifi(on: bool) {
    audio::spawn(&["sudo", "-n", HELPER, "wifi", if on { "on" } else { "off" }]);
}

fn set_bluetooth(on: bool) {
    gio::spawn_blocking(move || {
        let _ = bluetooth::set_power(on);
    });
}

fn set_night(on: bool) {
    if on {
        audio::spawn(&["hyprsunset", "-t", NIGHT_TEMPERATURE]);
    } else {
        audio::spawn(&["pkill", "-x", "hyprsunset"]);
    }
}
