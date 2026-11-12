//! A sidebar: a full-height panel that grows out of the screen frame's left
//! or right edge, under the bar. The frame looks as if it thickened: the
//! panel is the frame's colour, and the hole's rounded edge, shadow and edge
//! line slide across with it.
//!
//! Resting the pointer in the bar's outer corner opens it; it closes a moment
//! after the pointer has left both the panel and the bar strip above it. It
//! also opens by command (see [`Sidebar::toggle`]); then it stays until the
//! pointer has been in and out, Escape, or the command again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::anim::{self, Tween, MORPH};
use crate::{config, hyprland, theme};

const OPEN_MS: f64 = 380.0;
/// Panel width before scaling, frame edge included.
const WIDTH: f64 = 340.0;
/// How long the pointer rests in the corner before the panel opens.
const INTENT: Duration = Duration::from_millis(150);
const CLOSE_DELAY: Duration = Duration::from_millis(300);
/// The corner's hot square, before scaling.
const REACH: f64 = 6.0;
/// The panel tucks this far up under the bar, hiding the frame's edge line
/// there so the band and the panel read as one surface.
const OVERLAP: f64 = 2.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

pub struct Sidebar {
    side: Side,
    window: gtk::ApplicationWindow,
    canvas: gtk::DrawingArea,
    stage: gtk::Fixed,
    content: RefCell<Option<gtk::Widget>>,
    openness: Rc<RefCell<Tween>>,
    open: Cell<bool>,
    monitor: RefCell<Option<gdk::Monitor>>,
    /// The pointer is over the panel, or over the corner or bar strip above it.
    in_panel: Cell<bool>,
    in_bar: Cell<bool>,
    /// Opened by command and not yet visited: hovering away doesn't close it.
    pinned: Cell<bool>,
    /// Bumped when the pointer moves, so only the latest intent or close timer acts.
    generation: Cell<u32>,
    /// Called on open (true) and close (false) with the content scale, to
    /// (re)build the content or stop its live work.
    on_open: RefCell<Option<Box<dyn Fn(bool, f64)>>>,
}

impl Sidebar {
    pub fn new(app: &gtk::Application, side: Side, namespace: &str) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some(namespace));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(if side == Side::Left { Edge::Left } else { Edge::Right }, true);
        window.set_exclusive_zone(-1);

        let canvas = gtk::DrawingArea::new();
        let stage = gtk::Fixed::new();
        stage.set_overflow(gtk::Overflow::Hidden);
        let layers = gtk::Overlay::new();
        layers.set_child(Some(&canvas));
        layers.add_overlay(&stage);
        window.set_child(Some(&layers));

        let sidebar = Rc::new(Self {
            side,
            window,
            canvas,
            stage,
            content: RefCell::default(),
            openness: Tween::shared(OPEN_MS, MORPH),
            open: Cell::new(false),
            monitor: RefCell::default(),
            in_panel: Cell::new(false),
            in_bar: Cell::new(false),
            pinned: Cell::new(false),
            generation: Cell::new(0),
            on_open: RefCell::default(),
        });

        sidebar.canvas.set_draw_func({
            let sidebar = Rc::downgrade(&sidebar);
            move |_, cr, w, h| {
                if let Some(sidebar) = sidebar.upgrade() {
                    sidebar.draw(cr, f64::from(w), f64::from(h));
                }
            }
        });
        theme::on_palette_change({
            let canvas = sidebar.canvas.downgrade();
            move || canvas.upgrade().into_iter().for_each(|c| c.queue_draw())
        });

        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let sidebar = Rc::downgrade(&sidebar);
            move |_, _, _| {
                if let Some(sidebar) = sidebar.upgrade() {
                    sidebar.in_panel.set(true);
                    sidebar.pinned.set(false);
                    sidebar.pointer_moved();
                }
            }
        });
        motion.connect_leave({
            let sidebar = Rc::downgrade(&sidebar);
            move |_| {
                if let Some(sidebar) = sidebar.upgrade() {
                    sidebar.in_panel.set(false);
                    sidebar.pointer_moved();
                }
            }
        });
        sidebar.window.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let sidebar = Rc::downgrade(&sidebar);
            move |_, key, _, _| {
                if key != gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                sidebar.upgrade().into_iter().for_each(|s| s.hide());
                glib::Propagation::Stop
            }
        });
        sidebar.window.add_controller(keys);
        sidebar
    }

    /// `f(true, scale)` as the panel opens, `f(false, scale)` once it has shut.
    pub fn connect_open(&self, f: impl Fn(bool, f64) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    /// The panel's content; it gets the panel's width, frame edge excluded.
    pub fn set_content(&self, content: &impl IsA<gtk::Widget>) {
        if let Some(old) = self.content.borrow_mut().take() {
            self.stage.remove(&old);
        }
        let content = content.clone().upcast::<gtk::Widget>();
        let s = self.scale();
        content.set_size_request(((WIDTH - config::FRAME_EDGE) * s).round() as i32, self.height());
        self.stage.put(&content, 0.0, 0.0);
        *self.content.borrow_mut() = Some(content);
        self.place(self.openness.borrow().value);
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        if !self.open.get() {
            *self.monitor.borrow_mut() = monitor;
        }
    }

    pub fn scale(&self) -> f64 {
        config::bar_scale(self.monitor.borrow().as_ref().map_or(1080, |m| m.geometry().height()))
    }

    /// Where the panel's surface starts: the bar's underside, less the overlap.
    fn top(&self) -> f64 {
        config::FRAME_EDGE + config::BAR_HEIGHT * self.scale() - OVERLAP
    }

    fn height(&self) -> i32 {
        let screen = self.monitor.borrow().as_ref().map_or(1080, |m| m.geometry().height());
        (f64::from(screen) - self.top()).round() as i32 - config::FRAME_EDGE.round() as i32
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.open.get() {
            self.hide();
        } else {
            self.pinned.set(true);
            self.show();
        }
    }

    /// The pointer moved over `monitor`'s bar, to `at` (logical px), or left it.
    pub fn track_bar(self: &Rc<Self>, monitor: &gdk::Monitor, at: Option<(f64, f64)>) {
        let same_monitor = self.monitor.borrow().as_ref() == Some(monitor);
        let width = f64::from(monitor.geometry().width());
        let s = config::bar_scale(monitor.geometry().height());
        let inside = at.is_some_and(|(x, y)| {
            let from_edge = if self.side == Side::Left { x } else { width - x };
            let corner = from_edge <= REACH * s && y <= REACH * s;
            // Once open, the bar strip above the panel keeps it open, so the
            // pointer can travel down from the corner.
            corner || (self.open.get() && same_monitor && from_edge <= WIDTH * s)
        });
        if inside == self.in_bar.get() {
            return;
        }
        self.in_bar.set(inside);
        if inside && !self.open.get() {
            *self.monitor.borrow_mut() = Some(monitor.clone());
            let generation = self.bump();
            let sidebar = Rc::downgrade(self);
            glib::timeout_add_local_once(INTENT, move || {
                if let Some(s) = sidebar.upgrade().filter(|s| s.generation.get() == generation && s.in_bar.get()) {
                    s.show();
                }
            });
        } else {
            self.pointer_moved();
        }
    }

    fn bump(&self) -> u32 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }

    /// Closes after a moment if the pointer is now off the panel and its strip.
    fn pointer_moved(self: &Rc<Self>) {
        let generation = self.bump();
        if !self.open.get() || self.pinned.get() || self.in_panel.get() || self.in_bar.get() {
            return;
        }
        let sidebar = Rc::downgrade(self);
        glib::timeout_add_local_once(CLOSE_DELAY, move || {
            if let Some(s) = sidebar.upgrade().filter(|s| s.generation.get() == generation) {
                // Mapping the panel sends the bar and the panel stray leaves
                // while the pointer rests in the corner, so ask where it is.
                if s.pointer_over() {
                    s.pointer_moved();
                } else {
                    s.hide();
                }
            }
        });
    }

    /// Whether the cursor is over the panel or the bar strip above it, per Hyprland.
    fn pointer_over(&self) -> bool {
        let Some(monitor) = self.monitor.borrow().clone() else { return false };
        let Some((x, y)) = hyprland::request("j/cursorpos").ok().and_then(|t| {
            let v: serde_json::Value = serde_json::from_str(&t).ok()?;
            Some((v["x"].as_f64()?, v["y"].as_f64()?))
        }) else {
            return false;
        };
        let g = monitor.geometry();
        let (x, y) = (x - f64::from(g.x()), y - f64::from(g.y()));
        let from_edge = if self.side == Side::Left { x } else { f64::from(g.width()) - x };
        (0.0..=f64::from(g.height())).contains(&y) && (0.0..=WIDTH * self.scale()).contains(&from_edge)
    }

    fn show(self: &Rc<Self>) {
        if self.open.replace(true) {
            return;
        }
        let s = self.scale();
        let monitor = self.monitor.borrow().clone();
        self.window.set_monitor(monitor.as_ref());
        self.window.set_margin(Edge::Top, self.top().round() as i32);
        let width = WIDTH * s + config::FRAME_RADIUS + config::SHADOW_SIZE + 2.0;
        self.window.set_default_size(width.ceil() as i32, self.height());
        self.stage.set_size_request(width.ceil() as i32, self.height());
        if let Some(f) = self.on_open.borrow().as_ref() {
            f(true, s);
        }
        self.window.present();
        self.animate(1.0);
    }

    pub fn hide(self: &Rc<Self>) {
        if self.open.replace(false) {
            self.pinned.set(false);
            self.bump();
            self.animate(0.0);
        }
    }

    fn animate(self: &Rc<Self>, target: f64) {
        let sidebar = Rc::downgrade(self);
        anim::animate_to(&self.canvas, &self.openness, target, move |_, t| {
            let Some(sidebar) = sidebar.upgrade() else { return };
            sidebar.place(t);
            if t <= 0.0 && !sidebar.open.get() {
                sidebar.window.set_visible(false);
                sidebar.in_panel.set(false);
                if let Some(f) = sidebar.on_open.borrow().as_ref() {
                    f(false, sidebar.scale());
                }
            }
        });
    }

    /// Where the hole's edge sits at openness `t`, from the screen edge.
    fn reach(&self, t: f64) -> f64 {
        let edge = config::FRAME_EDGE;
        edge + (WIDTH * self.scale() - edge) * t
    }

    /// Slides the content with the edge and limits input to the panel.
    fn place(&self, t: f64) {
        let reach = self.reach(t);
        let s = self.scale();
        let surface_w = f64::from(self.stage.width().max(self.stage.width_request()));
        let content_w = (WIDTH - config::FRAME_EDGE) * s;
        if let Some(content) = self.content.borrow().as_ref() {
            let x = match self.side {
                Side::Left => reach - content_w,
                Side::Right => surface_w - reach,
            };
            self.stage.move_(content, x, 0.0);
            content.set_opacity(t.clamp(0.0, 1.0));
        }
        self.canvas.queue_draw();
        if let Some(surface) = self.window.surface() {
            let (x, w) = match self.side {
                Side::Left => (0, reach.ceil() as i32),
                Side::Right => ((surface_w - reach).floor() as i32, reach.ceil() as i32),
            };
            surface.set_input_region(Some(&cairo::Region::create_rectangle(&cairo::RectangleInt::new(x, 0, w, self.height()))));
        }
    }

    /// The frame's left (or mirrored right) part with its hole edge at the
    /// current reach: fill, the shadow falling into the hole, the edge line.
    fn draw(&self, cr: &cairo::Context, w: f64, h: f64) {
        if self.side == Side::Right {
            cr.translate(w, 0.0);
            cr.scale(-1.0, 1.0);
        }
        let palette = theme::palette();
        let reach = self.reach(self.openness.borrow().value);
        let radius = config::FRAME_RADIUS;
        // The hole runs on past the surface's far side and from its top (the
        // bar's underside) to the bottom frame edge.
        let bottom = h - config::FRAME_EDGE;
        let hole = |cr: &cairo::Context, inset: f64| {
            theme::rounded_rect(cr, reach + inset, OVERLAP + inset, w + radius * 2.0, bottom - OVERLAP - 2.0 * inset, (radius - inset).max(0.0));
        };

        cr.set_fill_rule(cairo::FillRule::EvenOdd);
        cr.rectangle(0.0, 0.0, w, h);
        hole(cr, 0.0);
        theme::set_source(cr, palette.base);
        let _ = cr.fill();

        let _ = cr.save();
        hole(cr, 0.0);
        cr.clip();
        let size = config::SHADOW_SIZE;
        cr.set_line_width(1.0);
        for step in 0..size as i32 {
            let falloff = 1.0 - f64::from(step) / size;
            hole(cr, f64::from(step) + 0.5);
            cr.set_source_rgba(0.0, 0.0, 0.0, config::SHADOW_STRENGTH * 0.35 * falloff * falloff);
            let _ = cr.stroke();
        }
        let _ = cr.restore();

        hole(cr, 0.0);
        cr.set_line_width(config::FRAME_BORDER_WIDTH);
        theme::set_source(cr, palette.edge);
        let _ = cr.stroke();
    }
}
