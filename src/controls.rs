//! The right sidebar: quick controls over a tabbed glance. A clock and the
//! weather, quick toggles (Wi-Fi, Bluetooth, do not disturb, night light,
//! always on, microphone, airplane mode) in a box, a row at a time with the
//! rest behind a chevron, the apps (Hub, Screenshot) flat beneath them,
//! volume and brightness faders, then a tab rail
//! over notifications, the calendar, now playing, the forecast and the
//! screen recorder with its recordings.
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
use crate::hint;
use crate::inbox_popout::InboxPopout;
use crate::media_popout::MediaPopout;
use crate::popout::{self, Popout};
use crate::recorder::deck::Deck;
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
/// Toggles in a row; more wrap behind the chevron.
const PER_ROW: usize = 5;
/// The apps row: glyph, name, and the eclipse-shell command that opens it.
const APPS: &[(&str, &str, &[&str])] = &[("settings", "Hub", &["hub"]), ("screenshot_region", "Screenshot", &["screenshot"])];
const NIGHT_TEMPERATURE: &str = "4500";
/// Hyprland starts hypridle alongside the shell, so a remembered Always on
/// keeps stopping it for this long after startup.
const AWAKE_GRACE: Duration = Duration::from_secs(30);

/// What the toggles show. `None`: there's no such radio.
#[derive(Clone, Default, PartialEq)]
struct State {
    wifi: Option<bool>,
    bluetooth: Option<bool>,
    night: bool,
    /// hypridle is stopped, so the screen never dims, locks or sleeps.
    awake: bool,
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
    /// The toggles past the first row are showing; kept across opens and
    /// restarts (see [`expanded_path`]).
    expanded: Cell<bool>,
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
        let (h_inbox, h_calendar, h_media, h_weather, h_record) = (holder(), holder(), holder(), holder(), holder());
        let pane_popout = |h: &gtk::Box| Popout::pane(app, h, sidebar.hints(), 1.0);
        let (p_inbox, p_calendar, p_media, p_weather, p_record) = (pane_popout(&h_inbox), pane_popout(&h_calendar), pane_popout(&h_media), pane_popout(&h_weather), pane_popout(&h_record));
        let inbox = InboxPopout::with_popout(p_inbox.clone());
        let calendar = Calendar::with_popout(p_calendar.clone());
        let media = MediaPopout::with_popout(p_media.clone());
        let weather = WeatherPopout::with_popout(p_weather.clone());
        let close: Rc<dyn Fn()> = {
            let sidebar = Rc::downgrade(&sidebar);
            Rc::new(move || sidebar.upgrade().into_iter().for_each(|s| s.hide()))
        };
        let record = Deck::with_popout(p_record.clone(), close);
        let panes = vec![
            Pane { glyph: "notifications", name: "Inbox", popout: p_inbox, open: Box::new(move || inbox.toggle(0.0)) },
            Pane { glyph: "calendar_month", name: "Calendar", popout: p_calendar, open: Box::new(move || calendar.toggle(0.0)) },
            Pane { glyph: "music_note", name: "Media", popout: p_media, open: Box::new(move || media.toggle(0.0)) },
            Pane { glyph: "partly_cloudy_day", name: "Weather", popout: p_weather, open: Box::new(move || weather.toggle(0.0)) },
            Pane { glyph: "radio_button_checked", name: "Record", popout: p_record, open: Box::new(move || record.toggle()) },
        ];

        restore_awake();
        let controls = Rc::new(Self {
            sidebar,
            panes,
            chosen: Cell::new(0),
            open: Cell::new(false),
            generation: Cell::new(0),
            state: RefCell::default(),
            toggles: RefCell::default(),
            expanded: Cell::new(expanded_path().exists()),
            volume: RefCell::default(),
            brightness: RefCell::default(),
            rail: RefCell::default(),
            weather: RefCell::default(),
        });
        let holders = [h_inbox, h_calendar, h_media, h_weather, h_record];
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

    /// Opens the sidebar on the Record tab (the launcher's Screen Recorder).
    pub fn show_record(self: &Rc<Self>) {
        let Some(index) = self.panes.iter().position(|p| p.name == "Record") else { return };
        self.chosen.set(index);
        if self.open.get() {
            self.open_pane(index);
            self.show_rail(self.sidebar.scale());
        } else {
            self.sidebar.toggle();
        }
    }

    fn build(self: &Rc<Self>, s: f64, holders: &[gtk::Box; 5]) {
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
        // Apps under the toggles, flat where the toggles are boxed, so the
        // two never read as one set.
        column.append(&self.apps(s));

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
        let awake = state.awake;
        let always = tile(s, "coffee", "Always on", awake, act(Box::new(move || set_awake(!awake))));
        hint::float_below(&always, self.sidebar.hints(), s, move || {
            hint::note(s, "Keeps the screen from dimming, locking, turning off or sleeping while idle.", 200.0)
        });
        tiles.push(always);
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
        // A row of PER_ROW; the rest behind the chevron bar below it.
        let expanded = self.expanded.get();
        for (i, t) in tiles.iter().enumerate().take(if expanded { usize::MAX } else { PER_ROW }) {
            grid.attach(t, (i % PER_ROW) as i32, (i / PER_ROW) as i32, 1, 1);
        }
        // Keep the columns even when a row is short.
        for i in tiles.len()..PER_ROW {
            grid.attach(&gtk::Box::new(gtk::Orientation::Horizontal, 0), i as i32, 0, 1, 1);
        }
        let inner = gtk::Box::new(gtk::Orientation::Vertical, px(8.0));
        inner.set_margin_top(px(10.0));
        inner.set_margin_bottom(px(10.0));
        inner.set_margin_start(px(10.0));
        inner.set_margin_end(px(10.0));
        inner.append(&grid);
        if tiles.len() > PER_ROW {
            let weak = Rc::downgrade(self);
            inner.append(&chevron(s, expanded, move || {
                if let Some(c) = weak.upgrade() {
                    let open = !c.expanded.get();
                    c.expanded.set(open);
                    let path = expanded_path();
                    if open {
                        let _ = path.parent().map(std::fs::create_dir_all);
                        let _ = std::fs::write(&path, "");
                    } else {
                        let _ = std::fs::remove_file(&path);
                    }
                    c.show_toggles();
                }
            }));
        }
        // The toggles' box, which sets them apart from the apps below.
        let back = gtk::DrawingArea::new();
        back.set_draw_func(move |_, cr, w, h| {
            let palette = theme::palette();
            theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 12.0 * s);
            theme::set_source(cr, palette.raised.alpha(0.5));
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, palette.outline);
            let _ = cr.stroke();
        });
        let boxed = gtk::Overlay::new();
        boxed.set_child(Some(&back));
        boxed.add_overlay(&inner);
        boxed.set_measure_overlay(&inner, true);
        holder.append(&boxed);
    }

    /// The apps row: Eclipse's own apps, launched with the sidebar closed
    /// first, so a screenshot doesn't catch it.
    fn apps(self: &Rc<Self>, s: f64) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_homogeneous(true);
        row.set_margin_top(px(2.0));
        for (i, (glyph, name, args)) in APPS.iter().enumerate() {
            let weak = Rc::downgrade(self);
            let tile = app_tile(s, glyph, name, i > 0, move || {
                if let Some(c) = weak.upgrade() {
                    c.sidebar.hide();
                }
                // Once the sidebar has slid away.
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    if let Ok(exe) = std::env::current_exe() {
                        let exe = exe.display().to_string();
                        let mut argv = vec![exe.as_str()];
                        argv.extend_from_slice(args);
                        audio::spawn(&argv);
                    }
                });
            });
            row.append(&tile);
        }
        row
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
    // Glyph only; the name fades in along the bottom on hover, as ryoku's.
    back.set_size_request(-1, px(44.0));
    let icon = if on { theme::filled_icon_glyph(glyph, 17.0 * s, theme::palette().accent_lit) } else { theme::icon_glyph(glyph, 17.0 * s, ICON_DIM) };
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_can_target(false);
    let label = TextStyle::new(7.5 * s, pango::Weight::Semibold, CREAM).label(name);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
    label.set_valign(gtk::Align::End);
    label.set_margin_bottom(px(3.0));
    label.set_margin_start(px(3.0));
    label.set_margin_end(px(3.0));
    label.set_can_target(false);
    label.set_opacity(0.0);
    let tile = gtk::Overlay::new();
    tile.set_child(Some(&back));
    tile.add_overlay(&icon);
    tile.add_overlay(&label);
    tile.set_cursor_from_name(Some("pointer"));
    tile.update_property(&[gtk::accessible::Property::Label(name)]);
    popout::on_hover(&tile, {
        let (back, label) = (back.downgrade(), label.downgrade());
        move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
            label.upgrade().into_iter().for_each(|l| l.set_opacity(if inside { 1.0 } else { 0.0 }));
        }
    });
    popout::on_click(&tile, pressed);
    tile
}

/// Present while the toggles are expanded.
fn expanded_path() -> std::path::PathBuf {
    glib::user_state_dir().join("eclipse-shell").join("toggles-expanded")
}

/// The full-width bar under the toggles that shows the rest, or hides them.
fn chevron(s: f64, expanded: bool, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let hovered = Rc::new(Cell::new(false));
    let back = gtk::DrawingArea::builder().content_height((20.0 * s).round() as i32).build();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            if hovered.get() {
                theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 8.0 * s);
                theme::set_source(cr, theme::palette().raised);
                let _ = cr.fill_preserve();
                cr.set_line_width(1.0);
                theme::set_source(cr, theme::CHIP_EDGE);
                let _ = cr.stroke();
            }
        }
    });
    let icon = theme::icon_glyph(if expanded { "expand_less" } else { "expand_more" }, 15.0 * s, ICON_DIM);
    icon.set_can_target(false);
    let bar = gtk::Overlay::new();
    bar.set_child(Some(&back));
    bar.add_overlay(&icon);
    bar.set_cursor_from_name(Some("pointer"));
    bar.update_property(&[gtk::accessible::Property::Label(if expanded { "Fewer toggles" } else { "More toggles" })]);
    popout::on_hover(&bar, {
        let (back, icon) = (back.downgrade(), icon.downgrade());
        let glyph = if expanded { "expand_less" } else { "expand_more" };
        move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
            if let Some(icon) = icon.upgrade() {
                icon.set_attributes(theme::icon_glyph(glyph, 15.0 * s, if inside { CREAM } else { ICON_DIM }).attributes().as_ref());
            }
        }
    });
    popout::on_click(&bar, pressed);
    bar
}

/// An app on the apps row: an outlined square with its glyph, its name
/// under it, and (after the first) a hairline on its leading edge. Flat,
/// with no box, unlike the toggles.
fn app_tile(s: f64, glyph: &str, name: &str, divider: bool, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let hovered = Rc::new(Cell::new(false));
    let square = gtk::DrawingArea::builder().content_width(px(38.0)).content_height(px(38.0)).halign(gtk::Align::Center).build();
    square.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 10.0 * s);
            if hovered.get() {
                theme::set_source(cr, palette.raised);
                let _ = cr.fill_preserve();
            }
            cr.set_line_width(1.0);
            theme::set_source(cr, if hovered.get() { theme::CHIP_EDGE } else { palette.outline });
            let _ = cr.stroke();
        }
    });
    let icon = theme::icon_glyph(glyph, 16.0 * s, ICON_DIM);
    icon.set_can_target(false);
    let face = gtk::Overlay::new();
    face.set_child(Some(&square));
    face.add_overlay(&icon);
    face.set_halign(gtk::Align::Center);
    let label = TextStyle::new(9.5 * s, pango::Weight::Medium, SUBTLE).label(name);
    let column = gtk::Box::new(gtk::Orientation::Vertical, px(6.0));
    column.append(&face);
    column.append(&label);
    column.set_margin_top(px(2.0));
    column.set_margin_bottom(px(2.0));
    // The hairline between apps.
    let line = gtk::DrawingArea::builder().content_width(1).content_height(px(22.0)).halign(gtk::Align::Start).valign(gtk::Align::Start).margin_top(px(10.0)).can_target(false).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, theme::HAIR);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line.set_visible(divider);
    let tile = gtk::Overlay::new();
    tile.set_child(Some(&column));
    tile.add_overlay(&line);
    tile.set_cursor_from_name(Some("pointer"));
    tile.update_property(&[gtk::accessible::Property::Label(name)]);
    popout::on_hover(&tile, {
        let (square, icon, label) = (square.downgrade(), icon.downgrade(), label.downgrade());
        let glyph = glyph.to_owned();
        move |inside| {
            hovered.set(inside);
            square.upgrade().into_iter().for_each(|q| q.queue_draw());
            if let Some(icon) = icon.upgrade() {
                icon.set_attributes(theme::icon_glyph(&glyph, 16.0 * s, if inside { CREAM } else { ICON_DIM }).attributes().as_ref());
            }
            if let Some(label) = label.upgrade() {
                TextStyle::new(9.5 * s, pango::Weight::Medium, if inside { CREAM } else { SUBTLE }).apply(&label);
            }
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
        night: running("hyprsunset"),
        awake: !running("hypridle"),
        mic_muted: volume("@DEFAULT_AUDIO_SOURCE@").is_some_and(|(_, m)| m),
        volume: volume("@DEFAULT_AUDIO_SINK@"),
    }
}

fn running(name: &str) -> bool {
    Command::new("pgrep").args(["-x", name]).output().is_ok_and(|o| o.status.success())
}

/// Where Always on is remembered across logins: present while it's on.
fn awake_path() -> std::path::PathBuf {
    glib::user_state_dir().join("eclipse-shell").join("always-on")
}

/// Always on stops hypridle outright; off starts it again.
fn set_awake(on: bool) {
    let path = awake_path();
    if on {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(&path, "");
    } else {
        let _ = std::fs::remove_file(&path);
    }
    if on {
        audio::spawn(&["pkill", "-x", "hypridle"]);
    } else {
        audio::spawn(&["hypridle"]);
    }
}

/// Puts a remembered Always on back: stops hypridle each second until
/// [`AWAKE_GRACE`] has passed, as it may start after the shell does.
fn restore_awake() {
    if !awake_path().exists() {
        return;
    }
    let start = std::time::Instant::now();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        // Turned off meanwhile: leave the hypridle it started alone.
        if !awake_path().exists() {
            return glib::ControlFlow::Break;
        }
        audio::spawn(&["pkill", "-x", "hypridle"]);
        if start.elapsed() < AWAKE_GRACE { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
    });
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
