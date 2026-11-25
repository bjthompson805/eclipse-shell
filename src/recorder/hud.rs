//! The recording island: a control that melts out of the screen frame, in
//! the frame's own colour, so it reads as part of the shell (ryoku's
//! FloatingIsland and RecordHud). Before a recording it's the chooser:
//! region or whole monitor, desktop audio, mic, then Record or close. While
//! recording: a pulsing dot, the elapsed time, stop, speaker and mic mutes,
//! and tuck, which melts it back into the frame as a small nub. Hovering the
//! nub brings it out for a moment; its untuck button keeps it out.
//!
//! Docked, it's fused to a frame edge: the frame swells out to hold it, with
//! concave fillets where they meet. The six-dot grip pulls it free; as it
//! nears an edge a neck reaches from the frame to meet it, and let go, it
//! settles onto the nearest edge (vertical on the sides). Ryoku's melt took
//! 0.62 s and its settle 1.7 s; these take a fifth of a second. The webcam
//! toggle isn't ported.

use std::cell::{Cell, RefCell};
use std::f64::consts::{FRAC_PI_2, PI};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::{active, chooser_open, clock_label, elapsed, elapsed_text, icon_button, pulse_dot, region, starting, RED};
use crate::theme::{self, TextStyle, CREAM, FAINT, SUBTLE};
use crate::{config, hyprland, popout};

const NAMESPACE: &str = "eclipse-shell-recorder";
const MELT: Duration = Duration::from_millis(200);
const SETTLE: Duration = Duration::from_millis(220);
/// How far out of the frame the tucked nub shows, of the full depth.
const NUB: f64 = 0.18;
/// Within this many px of an edge, before scaling, the frame reaches for it.
const REACH: f64 = 90.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Side {
    Bottom,
    Top,
    Left,
    Right,
}

impl Side {
    fn horizontal(self) -> bool {
        matches!(self, Self::Bottom | Self::Top)
    }
}

/// An eased change of a value over `span`.
struct Tween {
    from: f64,
    to: f64,
    start: Instant,
    span: Duration,
}

impl Tween {
    fn at(&self) -> (f64, bool) {
        let t = (self.start.elapsed().as_secs_f64() / self.span.as_secs_f64()).min(1.0);
        // Ease in-out cubic.
        let e = if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 };
        (self.from + (self.to - self.from) * e, t >= 1.0)
    }
}

pub struct Hud {
    window: gtk::ApplicationWindow,
    fixed: gtk::Fixed,
    /// The island's body (and its neck and fillets), drawn in the frame's colour.
    shape: gtk::DrawingArea,
    body: gtk::Box,
    grip: RefCell<Option<gtk::Widget>>,
    s: Cell<f64>,
    monitor: RefCell<Option<gdk::Monitor>>,
    side: Cell<Side>,
    /// Where along its side the island starts; none centres it.
    along: Cell<Option<f64>>,
    /// How far out of the frame: 0 in, 1 out.
    melt: Cell<f64>,
    melt_tween: RefCell<Option<Tween>>,
    /// The island's top-left while dragged free or settling; none when docked.
    free: Cell<Option<(f64, f64)>>,
    drag_from: Cell<(f64, f64)>,
    settle: RefCell<Option<(Tween, (f64, f64), (f64, f64))>>,
    /// Tucked to the nub while recording.
    hidden: Cell<bool>,
    /// The nub was hovered open; held a moment after the pointer leaves.
    revealed: Cell<bool>,
    reveal_leave: Cell<u32>,
    sink_muted: Cell<bool>,
    mic_muted: Cell<bool>,
    /// What the body last showed, so a tick of the clock doesn't rebuild it.
    key: RefCell<String>,
    clock: RefCell<Option<gtk::Label>>,
}

impl Hud {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some(NAMESPACE));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        let fixed = gtk::Fixed::new();
        let shape = gtk::DrawingArea::new();
        shape.set_can_target(false);
        fixed.put(&shape, 0.0, 0.0);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fixed.put(&body, 0.0, 0.0);
        // The buttons' hover hints float over everything.
        let layers = gtk::Overlay::new();
        layers.set_child(Some(&fixed));
        layers.add_overlay(&crate::hint::layer());
        window.set_child(Some(&layers));

        let hud = Rc::new(Self {
            window,
            fixed,
            shape,
            body,
            grip: RefCell::default(),
            s: Cell::new(1.0),
            monitor: RefCell::default(),
            side: Cell::new(Side::Bottom),
            along: Cell::new(None),
            melt: Cell::new(0.0),
            melt_tween: RefCell::default(),
            free: Cell::new(None),
            drag_from: Cell::new((0.0, 0.0)),
            settle: RefCell::default(),
            hidden: Cell::new(false),
            revealed: Cell::new(false),
            reveal_leave: Cell::new(0),
            sink_muted: Cell::new(false),
            mic_muted: Cell::new(false),
            key: RefCell::default(),
            clock: RefCell::default(),
        });
        hud.shape.set_draw_func({
            let hud = Rc::downgrade(&hud);
            move |_, cr, _, _| {
                if let Some(hud) = hud.upgrade() {
                    hud.draw(cr);
                }
            }
        });
        // Each frame: the melt and settle animations, and the nub's pulse.
        hud.window.add_tick_callback({
            let hud = Rc::downgrade(&hud);
            move |_, _| {
                if let Some(hud) = hud.upgrade() {
                    hud.tick();
                }
                glib::ControlFlow::Continue
            }
        });
        // Hovering brings a tucked island out; leaving tucks it again after a moment.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let hud = Rc::downgrade(&hud);
            move |_, _, _| hud.upgrade().into_iter().for_each(|h| h.hover(true))
        });
        motion.connect_leave({
            let hud = Rc::downgrade(&hud);
            move |_| hud.upgrade().into_iter().for_each(|h| h.hover(false))
        });
        hud.window.add_controller(motion);
        hud.wire_drag();
        super::on_change({
            let hud = Rc::downgrade(&hud);
            move || hud.upgrade().into_iter().for_each(|h| h.update())
        });
        hud
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        if self.window.is_visible() {
            return;
        }
        let height = monitor.as_ref().map_or(1080, |m| m.geometry().height());
        self.s.set(config::bar_scale(height));
        self.key.borrow_mut().clear();
        *self.monitor.borrow_mut() = monitor;
    }

    fn hover(self: &Rc<Self>, inside: bool) {
        let generation = self.reveal_leave.get().wrapping_add(1);
        self.reveal_leave.set(generation);
        if inside {
            self.revealed.set(true);
            self.update();
        } else {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(Duration::from_millis(260), move || {
                if let Some(hud) = weak.upgrade().filter(|h| h.reveal_leave.get() == generation) {
                    hud.revealed.set(false);
                    hud.update();
                }
            });
        }
    }

    fn tucked(&self) -> bool {
        active() && self.hidden.get() && !self.revealed.get()
    }

    /// Shows what the state calls for: the content, and how far out.
    fn update(self: &Rc<Self>) {
        let recording = active() || starting();
        let want = recording || chooser_open();
        if !recording {
            self.hidden.set(false);
        }
        let target = if !want {
            0.0
        } else if self.tucked() {
            NUB
        } else {
            1.0
        };
        if want {
            let key = if recording {
                format!("rec {} {} {} {:?}", self.sink_muted.get(), self.mic_muted.get(), self.hidden.get(), self.side.get().horizontal())
            } else {
                let (desktop, mic) = super::audio_choices();
                format!("choose {} {desktop} {mic} {:?}", region().is_some(), self.side.get().horizontal())
            };
            if *self.key.borrow() != key {
                if key.starts_with("rec") && !self.key.borrow().starts_with("rec") {
                    self.read_mutes();
                }
                *self.key.borrow_mut() = key.clone();
                self.rebuild(&key);
            }
            if !self.window.is_visible() {
                // Comes out of the frame from nothing; Hyprland's own layer
                // animation would fight the melt.
                hyprland::eval(&format!("hl.layer_rule({{ name = \"{NAMESPACE}-noanim\", match = {{ namespace = \"^{NAMESPACE}$\" }}, no_anim = true }})"));
                self.melt.set(0.0);
                self.window.set_monitor(self.monitor.borrow().as_ref());
                self.window.present();
            }
        }
        if let Some(clock) = &*self.clock.borrow() {
            clock.set_text(&elapsed_text(elapsed()));
        }
        let current = self.melt_tween.borrow().as_ref().map_or(self.melt.get(), |t| t.to);
        if (current - target).abs() > 1e-6 && self.window.is_visible() {
            *self.melt_tween.borrow_mut() = Some(Tween { from: self.melt.get(), to: target, start: Instant::now(), span: MELT });
        }
        self.place();
    }

    fn tick(&self) {
        if let Some(tween) = self.melt_tween.borrow_mut().take_if(|t| t.at().1) {
            self.melt.set(tween.to);
            if tween.to <= 0.0 {
                self.window.set_visible(false);
                self.key.borrow_mut().clear();
                self.free.set(None);
                return;
            }
        }
        if let Some(tween) = &*self.melt_tween.borrow() {
            self.melt.set(tween.at().0);
        }
        let settled = self.settle.borrow().as_ref().map(|(tw, from, to)| {
            let (k, done) = tw.at();
            ((from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k), done)
        });
        if let Some((at, done)) = settled {
            self.free.set((!done).then_some(at));
            if done {
                *self.settle.borrow_mut() = None;
            }
        }
        if self.melt_tween.borrow().is_some() || settled.is_some() || self.tucked() {
            self.place();
        }
    }

    // ---- geometry -------------------------------------------------------------

    fn screen(&self) -> (f64, f64) {
        let w = self.window.width();
        let h = self.window.height();
        if w > 0 && h > 0 {
            return (f64::from(w), f64::from(h));
        }
        self.monitor.borrow().as_ref().map_or((1920.0, 1080.0), |m| (f64::from(m.geometry().width()), f64::from(m.geometry().height())))
    }

    /// The frame's inner edge on `side`, in the window's coordinates.
    fn edge(&self, side: Side) -> f64 {
        let (w, h) = self.screen();
        let fe = config::get().frame_edge;
        match side {
            Side::Bottom => h - fe,
            Side::Top => fe + config::get().bar_height * self.s.get(),
            Side::Left => fe,
            Side::Right => w - fe,
        }
    }

    fn size(&self) -> (f64, f64) {
        let (_, nat) = self.body.preferred_size();
        (f64::from(nat.width()), f64::from(nat.height()))
    }

    /// Where the island's top-left sits fully out, docked on `side`.
    fn dock(&self, side: Side) -> (f64, f64) {
        let (bw, bh) = self.size();
        let (w, h) = self.screen();
        let (lo_x, hi_x) = (self.edge(Side::Left), self.edge(Side::Right) - bw);
        let (lo_y, hi_y) = (self.edge(Side::Top), self.edge(Side::Bottom) - bh);
        match side {
            Side::Bottom | Side::Top => {
                let x = self.along.get().unwrap_or((w - bw) / 2.0).clamp(lo_x, hi_x.max(lo_x));
                (x, if side == Side::Bottom { self.edge(side) - bh } else { self.edge(side) })
            }
            Side::Left | Side::Right => {
                let y = self.along.get().unwrap_or((h - bh) / 2.0).clamp(lo_y, hi_y.max(lo_y));
                (if side == Side::Right { self.edge(side) - bw } else { self.edge(side) }, y)
            }
        }
    }

    /// The edge nearest a free island at (x, y), and the gap to it.
    fn nearest(&self, x: f64, y: f64) -> (Side, f64) {
        let (bw, bh) = self.size();
        [
            (Side::Bottom, self.edge(Side::Bottom) - (y + bh)),
            (Side::Top, y - self.edge(Side::Top)),
            (Side::Left, x - self.edge(Side::Left)),
            (Side::Right, self.edge(Side::Right) - (x + bw)),
        ]
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, g)| (s, g.max(0.0)))
        .unwrap_or((Side::Bottom, 0.0))
    }

    /// The island now, as the outline needs it: its side, its rect fully
    /// out, its gap from that side, and how much of it shows.
    fn geometry(&self) -> (Side, (f64, f64, f64, f64), f64, f64) {
        let (bw, bh) = self.size();
        match self.free.get() {
            Some((x, y)) => {
                let (side, gap) = self.nearest(x, y);
                (side, (x, y, bw, bh), gap, 1.0)
            }
            None => {
                let side = self.side.get();
                let (x, y) = self.dock(side);
                (side, (x, y, bw, bh), 0.0, self.melt.get())
            }
        }
    }

    /// Lays out the body, the outline's area and the input region.
    fn place(&self) {
        let (side, (x, y, bw, bh), gap, t) = self.geometry();
        let s = self.s.get();
        // The content shows once the island is mostly out.
        let shown = if self.free.get().is_some() { 1.0 } else { ((t - 0.55) / 0.45).clamp(0.0, 1.0) };
        self.body.set_opacity(shown);
        self.body.set_can_target(shown > 0.5);
        self.fixed.move_(&self.body, x.round(), y.round());

        // The outline's box: the island and its neck, fillets either side.
        let r = fillet(s);
        let (along, w_along, depth) = if side.horizontal() { (x, bw, bh) } else { (y, bh, bw) };
        let e = self.edge(side);
        let (u0, u1) = (along - r - 2.0, along + w_along + r + 2.0);
        let (v0, v1) = (-3.0, gap + depth * if self.free.get().is_some() { 1.0 } else { t } + 2.0);
        let corners = [(u0, v0), (u1, v0), (u0, v1), (u1, v1)].map(|(u, v)| to_screen(side, e, u, v));
        let (bx0, by0) = corners.iter().fold((f64::MAX, f64::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (bx1, by1) = corners.iter().fold((f64::MIN, f64::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        self.shape.set_size_request((bx1 - bx0).ceil() as i32, (by1 - by0).ceil() as i32);
        self.fixed.move_(&self.shape, bx0.floor(), by0.floor());
        self.shape.queue_draw();

        // Input: the island, or the nub and a strip of frame around it to pop it back.
        if let Some(surface) = self.window.surface() {
            let region = if self.tucked() {
                let reach = 46.0 * s;
                let pad = 44.0 * s;
                let rect = match side {
                    Side::Bottom => (x - pad, e - reach, bw + 2.0 * pad, reach + config::get().frame_edge),
                    Side::Top => (x - pad, e - config::get().frame_edge, bw + 2.0 * pad, reach + config::get().frame_edge),
                    Side::Left => (0.0, y - pad, e + reach, bh + 2.0 * pad),
                    Side::Right => (e - reach, y - pad, reach + config::get().frame_edge, bh + 2.0 * pad),
                };
                rect
            } else {
                (bx0, by0, bx1 - bx0, by1 - by0)
            };
            let r = cairo::RectangleInt::new(region.0.floor() as i32, region.1.floor() as i32, region.2.ceil() as i32, region.3.ceil() as i32);
            surface.set_input_region(Some(&cairo::Region::create_rectangle(&r)));
        }
    }

    fn draw(&self, cr: &cairo::Context) {
        let (side, (x, y, bw, bh), gap, t) = self.geometry();
        let s = self.s.get();
        let free = self.free.get().is_some();
        let (along, w_along, depth) = if side.horizontal() { (x, bw, bh) } else { (y, bh, bw) };
        let depth = if free { depth } else { depth * t };
        if depth < 0.5 {
            return;
        }
        // The shape's area sits where place() put it; draw in window coordinates.
        if let Some(p) = self.shape.compute_point(&self.fixed, &gtk::graphene::Point::new(0.0, 0.0)) {
            cr.translate(-f64::from(p.x()), -f64::from(p.y()));
        }
        let e = self.edge(side);
        cr.transform(canonical(side, e));
        let pull = if free { (1.0 - gap / (REACH * s)).clamp(0.0, 1.0).powi(2) } else { 1.0 };
        let palette = theme::palette();
        outline(cr, along, w_along, depth, gap, pull, 17.0 * s, fillet(s), 30.0 * s, true);
        theme::set_source(cr, palette.base);
        let _ = cr.fill();
        outline(cr, along, w_along, depth, gap, pull, 17.0 * s, fillet(s), 30.0 * s, false);
        cr.set_line_width(config::get().frame_border_width);
        theme::set_source(cr, palette.edge);
        let _ = cr.stroke();

        // The tucked nub's pulsing record dot.
        if !free && self.tucked() {
            let now = glib::monotonic_time() as f64 / 1e6;
            let alpha = 0.18 + 0.82 * (0.5 + 0.5 * (now * std::f64::consts::TAU / 1.24).cos());
            cr.arc(along + w_along / 2.0, depth / 2.0, 4.0 * s, 0.0, 2.0 * PI);
            theme::set_source(cr, RED.alpha(alpha));
            let _ = cr.fill();
        }
    }

    // ---- dragging ----------------------------------------------------------------

    /// The grip pulls the island free; let go, it settles onto the nearest edge.
    fn wire_drag(self: &Rc<Self>) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let dragging = Rc::new(Cell::new(false));
        drag.connect_drag_begin({
            let (hud, dragging) = (Rc::downgrade(self), dragging.clone());
            move |_, x, y| {
                let Some(hud) = hud.upgrade() else { return };
                let on_grip = hud.fixed.pick(x, y, gtk::PickFlags::DEFAULT).zip(hud.grip.borrow().clone()).is_some_and(|(w, g)| w == g || w.is_ancestor(&g));
                dragging.set(on_grip && hud.melt_tween.borrow().is_none());
                if dragging.get() {
                    *hud.settle.borrow_mut() = None;
                    let (_, (ix, iy, _, _), _, _) = hud.geometry();
                    hud.drag_from.set((ix, iy));
                    hud.free.set(Some((ix, iy)));
                }
            }
        });
        drag.connect_drag_update({
            let (hud, dragging) = (Rc::downgrade(self), dragging.clone());
            move |_, dx, dy| {
                let Some(hud) = hud.upgrade().filter(|_| dragging.get()) else { return };
                let (bw, bh) = hud.size();
                let (fx, fy) = hud.drag_from.get();
                let x = (fx + dx).clamp(hud.edge(Side::Left), (hud.edge(Side::Right) - bw).max(hud.edge(Side::Left)));
                let y = (fy + dy).clamp(hud.edge(Side::Top), (hud.edge(Side::Bottom) - bh).max(hud.edge(Side::Top)));
                hud.free.set(Some((x, y)));
                hud.place();
            }
        });
        drag.connect_drag_end({
            let hud = Rc::downgrade(self);
            move |_, _, _| {
                let Some(hud) = hud.upgrade().filter(|_| dragging.replace(false)) else { return };
                let Some((x, y)) = hud.free.get() else { return };
                let (side, _) = hud.nearest(x, y);
                let flips = side.horizontal() != hud.side.get().horizontal();
                hud.side.set(side);
                hud.along.set(Some(if side.horizontal() { x } else { y }));
                if flips {
                    // Vertical on the sides: lay the content out again first.
                    hud.key.borrow_mut().clear();
                    hud.update();
                }
                let to = hud.dock(side);
                *hud.settle.borrow_mut() = Some((Tween { from: 0.0, to: 1.0, start: Instant::now(), span: SETTLE }, (x, y), to));
            }
        });
        self.fixed.add_controller(drag);
    }

    // ---- content -------------------------------------------------------------------

    fn rebuild(self: &Rc<Self>, key: &str) {
        let s = self.s.get();
        let px = |v: f64| (v * s).round() as i32;
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        *self.clock.borrow_mut() = None;
        let vertical = !self.side.get().horizontal();
        let row = gtk::Box::new(if vertical { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, px(if vertical { 7.0 } else { 8.0 }));
        row.set_margin_start(px(10.0));
        row.set_margin_end(px(10.0));
        row.set_margin_top(px(7.0));
        row.set_margin_bottom(px(7.0));
        row.set_halign(gtk::Align::Center);
        self.body.append(&row);
        let grip = grip(s);
        *self.grip.borrow_mut() = Some(grip.clone().upcast());
        row.append(&grip);

        if key.starts_with("rec") {
            row.append(&pulse_dot(9.0 * s, RED));
            let clock = clock_label(13.0 * s);
            row.append(&clock);
            *self.clock.borrow_mut() = Some(clock);
            row.append(&icon_button(s, "stop", RED, "Stop recording", super::stop));
            let hud = Rc::downgrade(self);
            let (sink, mic) = (self.sink_muted.get(), self.mic_muted.get());
            row.append(&icon_button(s, if sink { "volume_off" } else { "volume_up" }, if sink { FAINT } else { CREAM }, "Mute the speakers", {
                let hud = hud.clone();
                move || hud.upgrade().into_iter().for_each(|h| h.toggle_mute("@DEFAULT_AUDIO_SINK@"))
            }));
            row.append(&icon_button(s, if mic { "mic_off" } else { "mic" }, if mic { FAINT } else { CREAM }, "Mute the microphone", {
                let hud = hud.clone();
                move || hud.upgrade().into_iter().for_each(|h| h.toggle_mute("@DEFAULT_AUDIO_SOURCE@"))
            }));
            // Tuck into the frame, or, hovered out of it, stay out.
            let tucked = self.hidden.get();
            let (glyph, hint) = if tucked { ("expand", "Keep out of the frame") } else { ("compress", "Tuck into the frame") };
            row.append(&icon_button(s, glyph, SUBTLE, hint, move || {
                if let Some(hud) = hud.upgrade() {
                    hud.hidden.set(!tucked);
                    hud.revealed.set(false);
                    hud.update();
                }
            }));
            return;
        }

        let picked = region().is_some();
        let (desktop, mic) = super::audio_choices();
        row.append(&icon_button(s, if picked { "crop_free" } else { "desktop_windows" }, if picked { CREAM } else { SUBTLE }, if picked { "Switch to full-monitor recording" } else { "Switch to region recording" }, super::toggle_region));
        row.append(&icon_button(s, if desktop { "volume_up" } else { "volume_off" }, if desktop { CREAM } else { SUBTLE }, "Record desktop audio", super::toggle_desktop_audio));
        row.append(&icon_button(s, if mic { "mic" } else { "mic_off" }, if mic { CREAM } else { SUBTLE }, "Record the microphone", super::toggle_mic));
        let divider = gtk::DrawingArea::builder()
            .content_width(if vertical { px(18.0) } else { 1 })
            .content_height(if vertical { 1 } else { px(18.0) })
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        divider.set_draw_func(|_, cr, w, h| {
            theme::set_source(cr, SUBTLE.alpha(0.35));
            cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
            let _ = cr.fill();
        });
        row.append(&divider);
        row.append(&record_button(s));
        row.append(&icon_button(s, "close", SUBTLE, "Close", super::close_chooser));
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

/// The concave fillets' radius where the island meets the frame.
fn fillet(s: f64) -> f64 {
    12.0 * s
}

/// Maps the canonical space of `side` (u along the edge, v out from it into
/// the screen, the edge at v = 0) to the window's.
fn canonical(side: Side, e: f64) -> cairo::Matrix {
    match side {
        Side::Bottom => cairo::Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, e),
        Side::Top => cairo::Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, e),
        Side::Left => cairo::Matrix::new(0.0, 1.0, 1.0, 0.0, e, 0.0),
        Side::Right => cairo::Matrix::new(0.0, 1.0, -1.0, 0.0, e, 0.0),
    }
}

fn to_screen(side: Side, e: f64, u: f64, v: f64) -> (f64, f64) {
    canonical(side, e).transform_point(u, v)
}

/// The island's outline in canonical space: a body `w` along the edge from
/// `u0`, `depth` deep, `gap` out from the edge. As `pull` rises (0 apart, 1
/// close) the two reach for each other, as ryoku's blobs did: a bump rises
/// from the frame while the island stretches toward it, each by half the gap
/// plus `smooth`, scaled by `pull`. Once they meet they're one shape, the
/// island fused to the frame with concave fillets either side. `fill`
/// closes the path under the edge, into the frame's band; otherwise the
/// edge stays open, for the edge line.
#[allow(clippy::too_many_arguments)]
fn outline(cr: &cairo::Context, u0: f64, w: f64, depth: f64, gap: f64, pull: f64, radius: f64, fillet: f64, smooth: f64, fill: bool) {
    let top = gap + depth;
    cr.new_path();
    if pull <= 0.001 {
        theme::rounded_rect(cr, u0, gap, w, depth, radius);
        return;
    }
    let reach = (gap / 2.0 + smooth) * pull;
    if 2.0 * reach >= gap {
        return fused(cr, u0, w, top, radius, fillet, fill);
    }
    // Apart: the island, stretched toward the frame...
    let low = gap - reach;
    theme::rounded_rect(cr, u0, low, w, top - low, radius);
    // ...and the frame's bump reaching up to it.
    fused(cr, u0, w, reach, radius, fillet, fill);
}

/// A body from the edge up to `top`, fused to the edge in concave fillets.
fn fused(cr: &cairo::Context, u0: f64, w: f64, top: f64, radius: f64, fillet: f64, fill: bool) {
    if top < 0.5 {
        return;
    }
    let f = fillet.min(top / 2.0);
    let r = radius.min(top - f).min(w / 2.0).max(0.0);
    let u1 = u0 + w;
    cr.new_sub_path();
    cr.move_to(u0 - f, 0.0);
    cr.arc(u0 - f, f, f, -FRAC_PI_2, 0.0);
    cr.line_to(u0, top - r);
    cr.arc_negative(u0 + r, top - r, r, PI, FRAC_PI_2);
    cr.line_to(u1 - r, top);
    cr.arc_negative(u1 - r, top - r, r, FRAC_PI_2, 0.0);
    cr.line_to(u1, f);
    cr.arc(u1 + f, f, f, PI, 1.5 * PI);
    if fill {
        // Under the edge line, into the frame's band.
        cr.line_to(u1 + f, -3.0);
        cr.line_to(u0 - f, -3.0);
        cr.close_path();
    }
}

async fn muted(node: &str) -> bool {
    crate::audio::output(&["wpctl", "get-volume", node]).await.is_some_and(|o| o.contains("MUTED"))
}

/// The six-dot grip that pulls the island free.
fn grip(s: f64) -> gtk::DrawingArea {
    let grip = gtk::DrawingArea::builder().content_width((16.0 * s).round() as i32).content_height((20.0 * s).round() as i32).valign(gtk::Align::Center).halign(gtk::Align::Center).build();
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
    grip
}

/// The chooser's labelled "Record" action: a record glyph and a caption on a
/// faint red tile.
fn record_button(s: f64) -> gtk::Overlay {
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
    let label = TextStyle::new(9.5 * s, pango::Weight::Semibold, RED).mono().tracking(0.8 * s).label("Record");
    row.append(&label);
    let button = gtk::Overlay::new();
    button.set_child(Some(&back));
    button.add_overlay(&row);
    button.set_measure_overlay(&row, true);
    button.set_valign(gtk::Align::Center);
    button.set_halign(gtk::Align::Center);
    button.set_cursor_from_name(Some("pointer"));
    crate::hint::float_label(&button, s, "Start recording");
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
