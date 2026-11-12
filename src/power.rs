//! Power menu: a card dropping from the bar's right end, with the wallpaper
//! across its top fading into the card, who is signed in and for how long,
//! and a row of session actions.
//!
//! Lock and sleep fire on a click. Logout, restart and shut down fire only
//! after a press held for HOLD_MS, the tile filling with the accent from
//! the bottom as it confirms; letting go early drains it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::popout::{Anchor, Popout};
use crate::theme::{self, TextStyle, BRIGHT, CREAM, FAINT, ICON_DIM, SUBTLE};
use crate::{config, hyprland, mark};

const NAMESPACE: &str = "eclipse-shell-power";
const HOLD_MS: f64 = 1100.0;
const DRAIN_MS: f64 = 180.0;
/// Device pixels of hero height to decode the wallpaper at: enough to
/// cover the card's hero on a 1.5x display without upscaling.
const HERO_DECODE_HEIGHT: i32 = 1000;
pub const LOCK: &str = "pgrep -qf '[l]ock_shell.qml' || $HOME/.local/share/quickshell-lockscreen/lock.sh";

#[derive(Clone, Copy)]
enum Action {
    Lock,
    Sleep,
    Logout,
    Restart,
    Shutdown,
}

impl Action {
    const ALL: [Action; 5] = [Action::Lock, Action::Sleep, Action::Logout, Action::Restart, Action::Shutdown];

    fn glyph(self) -> &'static str {
        match self {
            Action::Lock => "lock",
            Action::Sleep => "bedtime",
            Action::Logout => "logout",
            Action::Restart => "restart_alt",
            Action::Shutdown => "power_settings_new",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Action::Lock => "Lock",
            Action::Sleep => "Sleep",
            Action::Logout => "Logout",
            Action::Restart => "Restart",
            Action::Shutdown => "Shutdown",
        }
    }

    /// Destructive actions need a held press.
    fn needs_hold(self) -> bool {
        matches!(self, Action::Logout | Action::Restart | Action::Shutdown)
    }

    fn run(self) {
        match self {
            Action::Lock => spawn(&["sh", "-c", LOCK]),
            Action::Sleep => spawn(&["acpiconf", "-s", "3"]),
            Action::Logout => hyprland::exit(),
            Action::Restart => spawn(&["shutdown", "-r", "now"]),
            Action::Shutdown => spawn(&["shutdown", "-p", "now"]),
        }
    }
}

pub struct Power {
    popout: Rc<Popout>,
    built_for: Cell<f64>,
    wallpaper: Rc<RefCell<Option<cairo::ImageSurface>>>,
    /// Which file `wallpaper` was decoded from.
    wallpaper_path: Rc<RefCell<String>>,
    /// Redrawn when a new wallpaper finishes decoding.
    background: RefCell<Option<gtk::DrawingArea>>,
    uptime: gtk::Label,
}

impl Power {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let power = Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            built_for: Cell::new(0.0),
            wallpaper: Rc::default(),
            wallpaper_path: Rc::default(),
            background: RefCell::default(),
            uptime: gtk::Label::new(None),
        });
        power.load_wallpaper();
        power
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let s = self.popout.scale();
        if self.built_for.get() != s {
            self.build(s);
            self.built_for.set(s);
        }
        self.uptime.set_text(&format!("UP {}", uptime_text()));
        self.load_wallpaper();
        self.popout.show(Anchor::Right);
    }

    fn hide(&self) {
        let popout = self.popout.clone();
        popout.hide();
    }

    /// Loads the current wallpaper for the hero, unless it is already loaded.
    /// Decoding happens off the main thread at about the size the card
    /// shows it, so a large wallpaper never holds up opening the menu.
    fn load_wallpaper(&self) {
        let (wallpaper, loaded) = (self.wallpaper.clone(), self.wallpaper_path.clone());
        let background = self.background.borrow().as_ref().map(|b| b.downgrade());
        glib::spawn_future_local(async move {
            let Some(path) = crate::wallpaper::current().await else { return };
            if *loaded.borrow() == path {
                return;
            }
            let decoded = gio::spawn_blocking({
                let path = path.clone();
                move || crate::wallpaper::decode_scaled(&path, HERO_DECODE_HEIGHT)
            })
            .await;
            let Some((pixels, w, h)) = decoded.ok().flatten() else { return };
            let Ok(surface) = cairo::ImageSurface::create_for_data(pixels, cairo::Format::ARgb32, w, h, w * 4) else { return };
            *wallpaper.borrow_mut() = Some(surface);
            *loaded.borrow_mut() = path;
            if let Some(background) = background.and_then(|b| b.upgrade()) {
                background.queue_draw();
            }
        });
    }

    fn build(self: &Rc<Self>, s: f64) {
        let (card_w, hero_h, strip_h) = (640.0 * s, 260.0 * s, 132.0 * s);
        let radius = config::LAUNCHER_RADIUS;

        let background = gtk::DrawingArea::new();
        background.set_size_request(card_w.round() as i32, (hero_h + strip_h).round() as i32);
        background.set_draw_func({
            let wallpaper = self.wallpaper.clone();
            move |_, cr, w, h| draw_card(cr, f64::from(w), f64::from(h), hero_h, radius, s, wallpaper.borrow().as_ref())
        });
        theme::on_palette_change({
            let background = background.downgrade();
            move || background.upgrade().into_iter().for_each(|b| b.queue_draw())
        });

        // Identity on the left of the strip, actions on the right.
        let strip = gtk::Box::builder().valign(gtk::Align::End).height_request(strip_h.round() as i32).build();
        let identity = gtk::Box::builder().spacing((14.0 * s).round() as i32).valign(gtk::Align::Center).hexpand(true).margin_start((22.0 * s).round() as i32).build();
        identity.append(&mark::mark(30.0 * s));
        let lines = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing((4.0 * s).round() as i32).valign(gtk::Align::Center).build();
        let user = glib::user_name().to_string_lossy().into_owned();
        let host = glib::host_name().to_string();
        let mono = |px: f64, color| TextStyle::new(px * s, pango::Weight::Normal, color).mono();
        let name = TextStyle::new(22.0 * s, pango::Weight::Medium, BRIGHT).label(&capitalize(&user));
        let address = mono(11.0, SUBTLE).label(&format!("{user}@{host}"));
        mono(11.0, FAINT).tracking(1.0).apply(&self.uptime);
        for label in [&name, &address, &self.uptime] {
            label.set_xalign(0.0);
            lines.append(label);
        }
        identity.append(&lines);
        strip.append(&identity);

        let actions = gtk::Box::builder().spacing((6.0 * s).round() as i32).valign(gtk::Align::Center).margin_end((18.0 * s).round() as i32).build();
        for action in Action::ALL {
            actions.append(&self.tile(action, s));
        }
        strip.append(&actions);

        let content = gtk::Overlay::new();
        content.set_child(Some(&background));
        content.add_overlay(&strip);
        self.popout.set_content(&content);
        *self.background.borrow_mut() = Some(background);
    }

    /// One action tile: an outlined rounded square holding a glyph over a
    /// small caps label.
    fn tile(self: &Rc<Self>, action: Action, s: f64) -> gtk::DrawingArea {
        let (w, h) = ((64.0 * s).round() as i32, (68.0 * s).round() as i32);
        let tile = gtk::DrawingArea::builder().width_request(w).height_request(h).build();
        tile.set_cursor_from_name(Some("pointer"));
        let hovered = Rc::new(Cell::new(false));
        // How far the hold has filled the tile, 0..1; latched full once fired.
        let fill = Rc::new(Cell::new(0.0));
        let fired = Rc::new(Cell::new(false));

        tile.set_draw_func({
            let (hovered, fill, fired) = (hovered.clone(), fill.clone(), fired.clone());
            move |_, cr, w, h| {
                let level = if fired.get() { 1.0 } else { fill.get() };
                draw_tile(cr, f64::from(w), f64::from(h), s, action, hovered.get(), level);
            }
        });

        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let (tile, hovered) = (tile.downgrade(), hovered.clone());
            move |_, _, _| {
                hovered.set(true);
                tile.upgrade().into_iter().for_each(|t| t.queue_draw());
            }
        });
        motion.connect_leave({
            let (tile, hovered) = (tile.downgrade(), hovered.clone());
            move |_| {
                hovered.set(false);
                tile.upgrade().into_iter().for_each(|t| t.queue_draw());
            }
        });
        tile.add_controller(motion);

        let press = gtk::GestureClick::new();
        let holding = Rc::new(Cell::new(false));
        press.connect_pressed({
            let (power, tile, fill, fired, holding) = (Rc::downgrade(self), tile.downgrade(), fill.clone(), fired.clone(), holding.clone());
            move |_, _, _, _| {
                let Some(tile) = tile.upgrade() else { return };
                if !action.needs_hold() {
                    return;
                }
                fired.set(false);
                holding.set(true);
                let (power, fill, fired, holding) = (power.clone(), fill.clone(), fired.clone(), holding.clone());
                let last = Cell::new(None::<f64>);
                tile.add_tick_callback(move |tile, clock| {
                    let now = clock.frame_time() as f64 / 1000.0;
                    let dt = now - last.get().unwrap_or(now);
                    last.set(Some(now));
                    let level = if holding.get() { fill.get() + dt / HOLD_MS } else { fill.get() - dt / DRAIN_MS };
                    fill.set(level.clamp(0.0, 1.0));
                    tile.queue_draw();
                    if holding.get() && fill.get() >= 1.0 {
                        holding.set(false);
                        fired.set(true);
                        fill.set(0.0);
                        action.run();
                        power.upgrade().into_iter().for_each(|p| p.hide());
                        return glib::ControlFlow::Break;
                    }
                    if !holding.get() && fill.get() <= 0.0 {
                        return glib::ControlFlow::Break;
                    }
                    glib::ControlFlow::Continue
                });
            }
        });
        press.connect_released({
            let (power, holding) = (Rc::downgrade(self), holding.clone());
            move |_, _, _, _| {
                if action.needs_hold() {
                    holding.set(false);
                } else {
                    action.run();
                    power.upgrade().into_iter().for_each(|p| p.hide());
                }
            }
        });
        // Dragging off the tile lets go of the hold.
        press.connect_cancel(move |_, _| holding.set(false));
        tile.add_controller(press);
        tile
    }
}

fn draw_card(cr: &cairo::Context, w: f64, h: f64, hero_h: f64, radius: f64, s: f64, wallpaper: Option<&cairo::ImageSurface>) {
    let palette = theme::palette();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
    theme::set_source(cr, palette.base);
    let _ = cr.fill_preserve();
    let _ = cr.save();
    cr.clip();

    // Hero: the wallpaper, cropped to fill, fading into the card below.
    let inset = 3.0 * s;
    if let Some(surface) = wallpaper {
        let _ = cr.save();
        // Round only the top corners: a rounded box running past the hero, cut at its bottom.
        theme::rounded_rect(cr, inset, inset, w - 2.0 * inset, hero_h - inset + radius, (radius - inset).max(0.0));
        cr.clip();
        cr.rectangle(inset, inset, w - 2.0 * inset, hero_h - inset);
        cr.clip();
        let (iw, ih) = (f64::from(surface.width()), f64::from(surface.height()));
        let (bw, bh) = (w - 2.0 * inset, hero_h - inset);
        let scale = (bw / iw).max(bh / ih);
        cr.translate(inset + (bw - iw * scale) / 2.0, inset + (bh - ih * scale) / 2.0);
        cr.scale(scale, scale);
        let _ = cr.set_source_surface(surface, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Good);
        let _ = cr.paint();
        let _ = cr.restore();
    }
    let fade = cairo::LinearGradient::new(0.0, hero_h - 96.0 * s, 0.0, hero_h);
    let base = palette.base;
    fade.add_color_stop_rgba(0.0, base.r, base.g, base.b, 0.0);
    fade.add_color_stop_rgba(1.0, base.r, base.g, base.b, 1.0);
    cr.rectangle(0.0, hero_h - 96.0 * s, w, 96.0 * s);
    let _ = cr.set_source(&fade);
    let _ = cr.fill();

    theme::set_source(cr, palette.outline);
    cr.rectangle(0.0, hero_h.round(), w, 1.0);
    let _ = cr.fill();
    let _ = cr.restore();

    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius - 0.5);
    cr.set_line_width(1.0);
    theme::set_source(cr, palette.outline);
    let _ = cr.stroke();
}

fn draw_tile(cr: &cairo::Context, w: f64, h: f64, s: f64, action: Action, hovered: bool, level: f64) {
    let palette = theme::palette();
    let radius = 13.0 * s;
    let engaged = level > 0.0;
    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius);
    if hovered {
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
    }
    if engaged {
        let _ = cr.save();
        cr.clip_preserve();
        let top = h * (1.0 - level);
        let ramp = cairo::LinearGradient::new(0.0, h, 0.0, top);
        let (a, b) = (palette.accent, palette.accent_lit);
        ramp.add_color_stop_rgba(0.0, a.r, a.g, a.b, 0.7);
        ramp.add_color_stop_rgba(1.0, b.r, b.g, b.b, 0.15);
        cr.rectangle(0.0, top, w, h - top);
        let _ = cr.set_source(&ramp);
        let _ = cr.fill();
        let _ = cr.restore();
    }
    cr.set_line_width(1.0);
    theme::set_source(cr, if hovered || engaged { palette.edge } else { palette.outline });
    let _ = cr.stroke();

    let accent = if action.needs_hold() { palette.accent_lit } else { CREAM };
    let glyph_color = if engaged { BRIGHT } else if hovered { accent } else { ICON_DIM };
    let label_color = if engaged { BRIGHT } else if hovered { SUBTLE } else { FAINT };

    let glyph = pangocairo::functions::create_layout(cr);
    let style = TextStyle { family: config::ICON_FONT, ..TextStyle::new(22.0 * s, pango::Weight::Normal, glyph_color) };
    glyph.set_text(action.glyph());
    glyph.set_attributes(Some(&style.attrs()));
    let label = pangocairo::functions::create_layout(cr);
    label.set_text(&action.label().to_uppercase());
    label.set_attributes(Some(&TextStyle::new(10.0 * s, pango::Weight::Normal, label_color).mono().tracking(1.0).attrs()));

    let (_, g) = glyph.pixel_extents();
    let (_, l) = label.pixel_extents();
    let gap = 4.0 * s;
    let total = f64::from(g.height()) + gap + f64::from(l.height());
    let y = (h - total) / 2.0;
    cr.move_to((w - f64::from(g.width())) / 2.0, y);
    pangocairo::functions::show_layout(cr, &glyph);
    cr.move_to((w - f64::from(l.width())) / 2.0, y + f64::from(g.height()) + gap);
    pangocairo::functions::show_layout(cr, &label);
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// "3D 4H", "4H 12M" or "12M".
fn uptime_text() -> String {
    let boot = std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.split("sec = ").nth(1)?.split(',').next()?.trim().parse::<i64>().ok()
        });
    let Some(boot) = boot else { return String::from("?") };
    let minutes = (glib::real_time() / 1_000_000 - boot).max(0) / 60;
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (d, h) {
        (0, 0) => format!("{m}M"),
        (0, _) => format!("{h}H {m}M"),
        _ => format!("{d}D {h}H"),
    }
}

fn spawn(argv: &[&str]) {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    if let Ok(p) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
        p.wait_async(gio::Cancellable::NONE, |_| {});
    }
}
