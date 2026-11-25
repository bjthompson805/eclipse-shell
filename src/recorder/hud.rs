//! The recording HUD, a floating control near the bottom of the screen.
//! Before a recording it's the chooser: region or whole monitor, desktop
//! audio, mic, then Quick to start or close. While recording: a pulsing dot,
//! the elapsed time, stop, speaker and mic mutes, and hide, which tucks it
//! to a small nub that hovering brings back. The six-dot grip drags it.
//!
//! Ryoku's island melted into the screen frame as a blob and had a webcam
//! toggle; this one fades in and out and has neither.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::{active, chooser_open, clock_label, elapsed, elapsed_text, icon_button, pulse_dot, region, starting, RED};
use crate::theme::{self, TextStyle, CREAM, FAINT, SUBTLE};
use crate::{config, hyprland, popout};

/// Gap from the bottom of the screen when it first shows.
const BOTTOM_GAP: f64 = 28.0;

pub struct Hud {
    window: gtk::ApplicationWindow,
    s: Cell<f64>,
    body: gtk::Box,
    monitor: RefCell<Option<gdk::Monitor>>,
    /// Where it was dragged to, as its top-left within the monitor; none
    /// until a drag, so it starts at the bottom centre.
    placed: Cell<Option<(f64, f64)>>,
    /// Tucked to the nub while recording.
    hidden: Cell<bool>,
    /// The nub was hovered open; held a moment after the pointer leaves.
    revealed: Cell<bool>,
    reveal_leave: Cell<u32>,
    sink_muted: Cell<bool>,
    mic_muted: Cell<bool>,
    /// What the body last showed, so a tick of the clock doesn't rebuild it.
    shape: RefCell<String>,
    clock: RefCell<Option<gtk::Label>>,
}

impl Hud {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some("eclipse-shell-recorder"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        window.set_exclusive_zone(-1);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let back = gtk::DrawingArea::new();
        back.set_draw_func(|_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h / 2.0).min(17.0 * 2.0));
            theme::set_source(cr, palette.base);
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, palette.outline);
            let _ = cr.stroke();
        });
        let panel = gtk::Overlay::new();
        panel.set_child(Some(&back));
        panel.add_overlay(&body);
        panel.set_measure_overlay(&body, true);
        window.set_child(Some(&panel));

        let hud = Rc::new(Self {
            window,
            s: Cell::new(1.0),
            body,
            monitor: RefCell::default(),
            placed: Cell::new(None),
            hidden: Cell::new(false),
            revealed: Cell::new(false),
            reveal_leave: Cell::new(0),
            sink_muted: Cell::new(false),
            mic_muted: Cell::new(false),
            shape: RefCell::default(),
            clock: RefCell::default(),
        });
        // Hovering the nub brings the controls back; leaving tucks them away
        // again after a moment.
        popout::on_hover(&panel, {
            let hud = Rc::downgrade(&hud);
            move |inside| {
                let Some(hud) = hud.upgrade() else { return };
                let generation = hud.reveal_leave.get().wrapping_add(1);
                hud.reveal_leave.set(generation);
                if inside {
                    hud.revealed.set(true);
                    hud.update();
                } else {
                    let weak = Rc::downgrade(&hud);
                    glib::timeout_add_local_once(Duration::from_millis(260), move || {
                        if let Some(hud) = weak.upgrade().filter(|h| h.reveal_leave.get() == generation) {
                            hud.revealed.set(false);
                            hud.update();
                        }
                    });
                }
            }
        });
        super::on_change({
            let hud = Rc::downgrade(&hud);
            move || {
                if let Some(hud) = hud.upgrade() {
                    hud.update();
                }
            }
        });
        hud
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        if self.window.is_visible() {
            return;
        }
        let height = monitor.as_ref().map_or(1080, |m| m.geometry().height());
        self.s.set(config::bar_scale(height));
        self.shape.borrow_mut().clear();
        *self.monitor.borrow_mut() = monitor;
    }

    fn tucked(&self) -> bool {
        active() && self.hidden.get() && !self.revealed.get()
    }

    /// Shows what the state calls for, rebuilding only when that changed.
    fn update(self: &Rc<Self>) {
        let recording = active() || starting();
        let want = recording || chooser_open();
        if !recording {
            self.hidden.set(false);
        }
        if !want {
            if self.window.is_visible() {
                self.window.set_visible(false);
            }
            self.shape.borrow_mut().clear();
            return;
        }
        let shape = if self.tucked() {
            "nub".to_owned()
        } else if recording {
            format!("rec {} {}", self.sink_muted.get(), self.mic_muted.get())
        } else {
            let (desktop, mic) = super::audio_choices();
            format!("choose {} {desktop} {mic}", region().is_some())
        };
        if *self.shape.borrow() != shape {
            let was_visible = self.window.is_visible();
            if shape.starts_with("rec") && !self.shape.borrow().starts_with("rec") {
                self.read_mutes();
            }
            *self.shape.borrow_mut() = shape.clone();
            self.rebuild(&shape);
            if !was_visible {
                self.window.set_monitor(self.monitor.borrow().as_ref());
                self.window.set_opacity(0.0);
                self.window.present();
                let window = self.window.downgrade();
                let start = Cell::new(None::<i64>);
                self.window.add_tick_callback(move |_, clock| {
                    let now = clock.frame_time();
                    let t0 = *start.get().get_or_insert(now);
                    let t = ((now - t0) as f64 / 180_000.0).min(1.0);
                    if let Some(w) = window.upgrade() {
                        w.set_opacity(t);
                    }
                    if t >= 1.0 { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
                });
            }
            self.place();
        }
        if let Some(clock) = &*self.clock.borrow() {
            clock.set_text(&elapsed_text(elapsed()));
        }
    }

    /// Puts it where it was dragged, else at the bottom centre.
    fn place(&self) {
        let (mw, mh) = self.monitor.borrow().as_ref().map_or((1920, 1080), |m| (m.geometry().width(), m.geometry().height()));
        let (w, h) = (self.window.measure(gtk::Orientation::Horizontal, -1).1, self.window.measure(gtk::Orientation::Vertical, -1).1);
        let (x, y) = self.placed.get().unwrap_or_else(|| (f64::from(mw - w) / 2.0, f64::from(mh - h) - BOTTOM_GAP * self.s.get()));
        let x = x.clamp(0.0, f64::from((mw - w).max(0)));
        let y = y.clamp(0.0, f64::from((mh - h).max(0)));
        self.window.set_margin(Edge::Left, x.round() as i32);
        self.window.set_margin(Edge::Top, y.round() as i32);
    }

    fn rebuild(self: &Rc<Self>, shape: &str) {
        let s = self.s.get();
        let px = |v: f64| (v * s).round() as i32;
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        *self.clock.borrow_mut() = None;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        row.set_margin_start(px(10.0));
        row.set_margin_end(px(10.0));
        row.set_margin_top(px(7.0));
        row.set_margin_bottom(px(7.0));
        self.body.append(&row);

        if shape == "nub" {
            row.set_margin_start(px(8.0));
            row.set_margin_end(px(8.0));
            row.append(&pulse_dot(8.0 * s, RED));
            return;
        }
        row.append(&self.grip());
        if shape.starts_with("rec") {
            row.append(&pulse_dot(9.0 * s, RED));
            let clock = clock_label(13.0 * s);
            row.append(&clock);
            *self.clock.borrow_mut() = Some(clock);
            row.append(&icon_button(s, "stop", RED, "Stop recording", super::stop));
            let hud = Rc::downgrade(self);
            let (sink, mic) = (self.sink_muted.get(), self.mic_muted.get());
            row.append(&icon_button(s, if sink { "volume_off" } else { "volume_up" }, if sink { FAINT } else { CREAM }, "Mute the speakers", {
                let hud = hud.clone();
                move || {
                    if let Some(hud) = hud.upgrade() {
                        hud.toggle_mute("@DEFAULT_AUDIO_SINK@");
                    }
                }
            }));
            row.append(&icon_button(s, if mic { "mic_off" } else { "mic" }, if mic { FAINT } else { CREAM }, "Mute the microphone", {
                let hud = hud.clone();
                move || {
                    if let Some(hud) = hud.upgrade() {
                        hud.toggle_mute("@DEFAULT_AUDIO_SOURCE@");
                    }
                }
            }));
            row.append(&icon_button(s, "compress", SUBTLE, "Hide", move || {
                if let Some(hud) = hud.upgrade() {
                    hud.hidden.set(!hud.hidden.get());
                    hud.revealed.set(false);
                    hud.update();
                }
            }));
            return;
        }

        let picked = region().is_some();
        let (desktop, mic) = super::audio_choices();
        row.append(&icon_button(s, if picked { "crop_free" } else { "desktop_windows" }, if picked { CREAM } else { SUBTLE }, if picked { "Region (click for the whole monitor)" } else { "Whole monitor (click to pick a region)" }, super::toggle_region));
        row.append(&icon_button(s, if desktop { "volume_up" } else { "volume_off" }, if desktop { CREAM } else { SUBTLE }, "Record desktop audio", super::toggle_desktop_audio));
        row.append(&icon_button(s, if mic { "mic" } else { "mic_off" }, if mic { CREAM } else { SUBTLE }, "Record the microphone", super::toggle_mic));
        let divider = gtk::DrawingArea::builder().content_width(1).content_height(px(18.0)).valign(gtk::Align::Center).build();
        divider.set_draw_func(|_, cr, w, h| {
            theme::set_source(cr, SUBTLE.alpha(0.35));
            cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
            let _ = cr.fill();
        });
        row.append(&divider);
        row.append(&quick_button(s));
        row.append(&icon_button(s, "close", SUBTLE, "Close", super::close_chooser));
    }

    /// The six-dot grip that drags the HUD. The pointer is read back from
    /// Hyprland, as the surface moves under it.
    fn grip(self: &Rc<Self>) -> gtk::DrawingArea {
        let s = self.s.get();
        let grip = gtk::DrawingArea::builder().content_width((16.0 * s).round() as i32).content_height((20.0 * s).round() as i32).valign(gtk::Align::Center).build();
        let hovered = Rc::new(Cell::new(false));
        grip.set_draw_func({
            let hovered = hovered.clone();
            move |_, cr, w, h| {
                let (w, h) = (f64::from(w), f64::from(h));
                let (r, gap) = (1.5 * s, 3.0 * s);
                theme::set_source(cr, if hovered.get() { CREAM } else { SUBTLE });
                for col in 0..2 {
                    for line in 0..3 {
                        let x = w / 2.0 + (f64::from(col) - 0.5) * (2.0 * r + gap);
                        let y = h / 2.0 + f64::from(line - 1) * (2.0 * r + gap);
                        cr.arc(x, y, r, 0.0, std::f64::consts::TAU);
                        let _ = cr.fill();
                    }
                }
            }
        });
        grip.set_cursor_from_name(Some("move"));
        popout::on_hover(&grip, {
            let grip = grip.downgrade();
            move |inside| {
                hovered.set(inside);
                grip.upgrade().into_iter().for_each(|g| g.queue_draw());
            }
        });
        let drag = gtk::GestureDrag::new();
        let grab = Rc::new(Cell::new((0.0, 0.0)));
        drag.connect_drag_begin({
            let (hud, grab) = (Rc::downgrade(self), grab.clone());
            move |_, _, _| {
                let Some(hud) = hud.upgrade() else { return };
                let (Some((cx, cy)), Some((ox, oy))) = (cursor(), hud.origin()) else { return };
                let (x, y) = (f64::from(hud.window.margin(Edge::Left)), f64::from(hud.window.margin(Edge::Top)));
                grab.set((cx - ox - x, cy - oy - y));
            }
        });
        drag.connect_drag_update({
            let hud = Rc::downgrade(self);
            move |_, _, _| {
                let Some(hud) = hud.upgrade() else { return };
                let (Some((cx, cy)), Some((ox, oy))) = (cursor(), hud.origin()) else { return };
                let (gx, gy) = grab.get();
                hud.placed.set(Some((cx - ox - gx, cy - oy - gy)));
                hud.place();
            }
        });
        grip.add_controller(drag);
        grip
    }

    /// The monitor's top-left in Hyprland's layout.
    fn origin(&self) -> Option<(f64, f64)> {
        let g = self.monitor.borrow().as_ref()?.geometry();
        Some((f64::from(g.x()), f64::from(g.y())))
    }

    fn read_mutes(self: &Rc<Self>) {
        let hud = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let sink = muted("@DEFAULT_AUDIO_SINK@").await;
            let mic = muted("@DEFAULT_AUDIO_SOURCE@").await;
            if let Some(hud) = hud.upgrade() {
                hud.sink_muted.set(sink);
                hud.mic_muted.set(mic);
                hud.update();
            }
        });
    }

    fn toggle_mute(self: &Rc<Self>, node: &'static str) {
        let hud = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let _ = crate::audio::output(&["wpctl", "set-mute", node, "toggle"]).await;
            if let Some(hud) = hud.upgrade() {
                hud.read_mutes();
            }
        });
    }
}

async fn muted(node: &str) -> bool {
    crate::audio::output(&["wpctl", "get-volume", node]).await.is_some_and(|o| o.contains("MUTED"))
}

/// The pointer, in Hyprland's layout coordinates.
fn cursor() -> Option<(f64, f64)> {
    let text = hyprland::request("cursorpos").ok()?;
    let (x, y) = text.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// The chooser's labelled "Quick" action: a record glyph and a caption on a
/// faint red tile.
fn quick_button(s: f64) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let hovered = Rc::new(Cell::new(false));
    let back = gtk::DrawingArea::new();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            theme::rounded_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), 7.0 * s);
            theme::set_source(cr, if hovered.get() { theme::palette().raised } else { RED.alpha(0.16) });
            let _ = cr.fill();
        }
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(5.0));
    row.set_margin_start(px(7.0));
    row.set_margin_end(px(7.0));
    row.set_margin_top(px(4.0));
    row.set_margin_bottom(px(4.0));
    row.append(&theme::filled_icon_glyph("fiber_manual_record", 14.0 * s, RED));
    let label = TextStyle::new(9.5 * s, pango::Weight::Semibold, RED).mono().tracking(0.8 * s).label("Quick");
    row.append(&label);
    let button = gtk::Overlay::new();
    button.set_child(Some(&back));
    button.add_overlay(&row);
    button.set_measure_overlay(&row, true);
    button.set_valign(gtk::Align::Center);
    button.set_cursor_from_name(Some("pointer"));
    button.set_tooltip_text(Some("Start recording"));
    popout::on_hover(&button, {
        let back = back.downgrade();
        move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
        }
    });
    popout::on_click(&button, super::start_quick);
    button
}
