//! On-screen display: a small panel under the middle of the bar that shows a
//! change for a moment, such as Caps Lock turning on, then fades. It never
//! takes a click or the keyboard and reserves no space.
//!
//! Levels: the volume and brightness key binds run `eclipse-shell osd
//! volume|brightness` after making their change; the new level is read back
//! (wpctl, backlight) and shown as a bar.
//!
//! Lock keys: Hyprland runs `eclipse-shell lock-keys caps|num` as Caps Lock
//! or Num Lock is pressed (see the user's lock_keys.lua; release binds never
//! fire for them). FreeBSD has no LED devices to watch and Hyprland's devices
//! report never changes, so the shell keeps the state itself: read once at
//! startup, then flipped on every press.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::anim::{self, Easing, Tween, MORPH};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, HAIR, ICON_DIM};
use crate::{config, hyprland};

const SHOW_FOR: Duration = Duration::from_millis(1400);
const FADE_MS: f64 = 160.0;
const WIDTH: f64 = 248.0;
const HEIGHT: f64 = 44.0;

pub struct Osd {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    fade: Rc<RefCell<Tween>>,
    /// Bumped on every flash, so only the latest one's timer hides the panel.
    generation: Cell<u32>,
    monitor: RefCell<Option<gdk::Monitor>>,
    /// Fading in or held, rather than fading out.
    showing: Cell<bool>,
    caps_on: Cell<bool>,
    /// Bumped per level read, so a slow read can't overwrite a newer one.
    reads: Cell<u32>,
    num_on: Cell<bool>,
}

impl Osd {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some("eclipse-shell-osd"));
        // Over everything, fullscreen windows included.
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_anchor(Edge::Top, true);
        window.set_exclusive_zone(-1);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let back = gtk::DrawingArea::new();
        back.set_draw_func(|_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
            theme::set_source(cr, palette.base);
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, palette.outline);
            let _ = cr.stroke();
        });
        let panel = gtk::Overlay::new();
        panel.set_child(Some(&back));
        panel.add_overlay(&row);
        window.set_child(Some(&panel));
        // Clicks fall through to whatever is beneath.
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&cairo::Region::create()));
            }
        });

        let osd = Rc::new(Self { window, row, fade: Tween::shared(FADE_MS, MORPH), generation: Cell::new(0), monitor: RefCell::default(), showing: Cell::new(false), caps_on: Cell::new(false), reads: Cell::new(0), num_on: Cell::new(false) });
        if let Some((caps, num)) = lock_keys() {
            osd.caps_on.set(caps);
            osd.num_on.set(num);
        }
        osd
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        *self.monitor.borrow_mut() = monitor;
    }

    /// Caps Lock (`caps`) or Num Lock was just pressed: flip and show its state.
    pub fn lock_key(self: &Rc<Self>, caps: bool) {
        let (state, glyph, name) = if caps { (&self.caps_on, "keyboard_capslock", "Caps Lock") } else { (&self.num_on, "dialpad", "Num Lock") };
        state.set(!state.get());
        self.flash_lock(glyph, name, state.get());
    }

    /// The volume or brightness just changed: read it back and show it.
    pub fn level(self: &Rc<Self>, volume: bool) {
        let read = self.reads.get() + 1;
        self.reads.set(read);
        let osd = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some((value, muted)) = read_level(volume).await else { return };
            let Some(o) = osd.upgrade().filter(|o| o.reads.get() == read) else { return };
            let glyph = match value {
                _ if volume && muted => "volume_off",
                v if volume && v > 0.5 => "volume_up",
                v if volume && v > 0.0 => "volume_down",
                _ if volume => "volume_mute",
                v if v > 0.66 => "brightness_high",
                v if v > 0.33 => "brightness_medium",
                _ => "brightness_low",
            };
            o.flash_level(glyph, value, muted);
        });
    }

    fn flash_level(self: &Rc<Self>, glyph: &str, value: f64, muted: bool) {
        let s = self.scale();
        let px = |v: f64| (v * s).round() as i32;
        self.clear_row(s);
        self.row.append(&theme::filled_icon_glyph(glyph, 17.0 * s, if muted { FAINT } else { CREAM }));
        let bar = gtk::DrawingArea::new();
        bar.set_hexpand(true);
        bar.set_content_height(px(4.0));
        bar.set_valign(gtk::Align::Center);
        bar.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
            theme::set_source(cr, HAIR);
            let _ = cr.fill();
            let filled = (w * value.clamp(0.0, 1.0)).max(if value > 0.0 { h } else { 0.0 });
            if filled > 0.0 {
                theme::rounded_rect(cr, 0.0, 0.0, filled, h, h / 2.0);
                theme::set_source(cr, if muted { FAINT } else { theme::palette().accent_lit });
                let _ = cr.fill();
            }
        });
        self.row.append(&bar);
        let percent = TextStyle::new(11.0 * s, pango::Weight::Semibold, if muted { DIM } else { CREAM }).mono().label(&format!("{:.0}", value * 100.0));
        // Room for "100", so the bar doesn't twitch as the digits change.
        percent.set_width_chars(3);
        percent.set_xalign(1.0);
        self.row.append(&percent);
        self.show();
    }

    fn clear_row(&self, s: f64) {
        let px = |v: f64| (v * s).round() as i32;
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        self.row.set_spacing(px(12.0));
        self.row.set_margin_start(px(16.0));
        self.row.set_margin_end(px(18.0));
    }

    fn flash_lock(self: &Rc<Self>, glyph: &str, name: &str, on: bool) {
        let s = self.scale();
        self.clear_row(s);
        self.row.append(&theme::icon_glyph(glyph, 17.0 * s, if on { CREAM } else { ICON_DIM }));
        let label = TextStyle::new(11.5 * s, pango::Weight::Semibold, CREAM).label(name);
        label.set_hexpand(true);
        label.set_xalign(0.0);
        self.row.append(&label);
        let state = if on { TextStyle::new(11.0 * s, pango::Weight::Bold, theme::palette().accent_lit) } else { TextStyle::new(11.0 * s, pango::Weight::Semibold, DIM) };
        self.row.append(&state.tracking(1.0 * s).label(if on { "ON" } else { "OFF" }));
        self.show();
    }

    fn scale(&self) -> f64 {
        config::bar_scale(self.monitor.borrow().as_ref().map_or(1080, |m| m.geometry().height()))
    }

    /// Shows the panel as it now reads, and hides it after a moment.
    fn show(self: &Rc<Self>) {
        let s = self.scale();
        self.window.set_monitor(self.monitor.borrow().as_ref());
        self.window.set_margin(Edge::Top, (config::FRAME_EDGE + config::BAR_HEIGHT * s + 10.0 * s).round() as i32);
        self.window.set_default_size((WIDTH * s).round() as i32, (HEIGHT * s).round() as i32);
        // On the backdrop, not the row: the row's margins sit inside this size.
        let panel = self.window.child().and_downcast::<gtk::Overlay>().expect("the panel");
        panel.child().expect("the backdrop").set_size_request((WIDTH * s).round() as i32, (HEIGHT * s).round() as i32);
        self.showing.set(true);
        self.window.present();
        self.animate(1.0);

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let osd = Rc::downgrade(self);
        glib::timeout_add_local_once(SHOW_FOR, move || {
            if let Some(o) = osd.upgrade().filter(|o| o.generation.get() == generation) {
                o.showing.set(false);
                o.animate(0.0);
            }
        });
    }

    fn animate(self: &Rc<Self>, target: f64) {
        let osd = Rc::downgrade(self);
        let content = self.window.child().expect("the panel");
        anim::animate_to(&content, &self.fade, target, move |panel, t| {
            panel.set_opacity(Easing::OutCubic.apply(t));
            if let Some(o) = osd.upgrade().filter(|o| t <= 0.0 && !o.showing.get()) {
                o.window.set_visible(false);
            }
        });
    }
}

/// Caps Lock and Num Lock on Hyprland's main keyboard.
fn lock_keys() -> Option<(bool, bool)> {
    let devices: serde_json::Value = serde_json::from_str(&hyprland::request("j/devices").ok()?).ok()?;
    let main = devices["keyboards"].as_array()?.iter().find(|k| k["main"].as_bool() == Some(true))?;
    Some((main["capsLock"].as_bool()?, main["numLock"].as_bool()?))
}

/// The default sink's volume and mute, or the backlight (never muted), 0..1.
async fn read_level(volume: bool) -> Option<(f64, bool)> {
    let argv: &[&str] = if volume { &["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"] } else { &["backlight", "-q"] };
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    let stdout = stdout?;
    if volume {
        // "Volume: 0.34" or "Volume: 0.34 [MUTED]"
        let value: f64 = stdout.split_whitespace().nth(1)?.parse().ok()?;
        Some((value, stdout.contains("[MUTED]")))
    } else {
        Some((stdout.trim().parse::<f64>().ok()? / 100.0, false))
    }
}
