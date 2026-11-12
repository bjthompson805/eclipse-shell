//! A popout: a card dropping from the bar on its own full-screen overlay
//! surface. A click off the card or Escape closes it. Opening fades the card
//! in while it drops a few pixels into place.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::anim::{self, Easing, MORPH, Tween};
use crate::theme::{self, DIM, TextStyle};
use crate::{config, hint, mark};

const OPEN_MS: f64 = 260.0;
const DROP: f64 = 8.0;

/// Where the card sits along the bar.
#[derive(Clone, Copy)]
pub enum Anchor {
    /// Against the right end of the bar.
    Right,
    /// Centred under this x (logical px from the monitor's left edge),
    /// kept on screen.
    Under(f64),
}

pub struct Popout {
    window: gtk::ApplicationWindow,
    card: gtk::Box,
    /// Hover hints float here, over the card.
    hints: gtk::Fixed,
    openness: Rc<RefCell<Tween>>,
    open: Cell<bool>,
    monitor: RefCell<Option<gdk::Monitor>>,
    /// The card's resting distance from the top of the screen.
    top: Cell<f64>,
}

impl Popout {
    pub fn new(app: &gtk::Application, namespace: &str) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some(namespace));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);

        let catcher = gtk::DrawingArea::new();
        let card = gtk::Box::builder().valign(gtk::Align::Start).build();
        let layers = gtk::Overlay::new();
        layers.set_child(Some(&catcher));
        layers.add_overlay(&card);
        let hints = hint::layer();
        layers.add_overlay(&hints);
        window.set_child(Some(&layers));

        let popout = Rc::new(Self { window, card, hints, openness: Tween::shared(OPEN_MS, MORPH), open: Cell::new(false), monitor: RefCell::default(), top: Cell::new(0.0) });

        let click = gtk::GestureClick::new();
        click.connect_released({
            let popout = Rc::downgrade(&popout);
            move |_, _, _, _| popout.upgrade().into_iter().for_each(|p| p.hide())
        });
        catcher.add_controller(click);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let popout = Rc::downgrade(&popout);
            move |_, key, _, _| {
                if key != gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                popout.upgrade().into_iter().for_each(|p| p.hide());
                glib::Propagation::Stop
            }
        });
        popout.window.add_controller(keys);
        popout
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        *self.monitor.borrow_mut() = monitor;
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Logical width of the monitor the popout opens on next.
    pub fn monitor_width(&self) -> i32 {
        self.monitor.borrow().as_ref().map_or(1920, |m| m.geometry().width())
    }

    /// The content scale on the monitor the popout opens on next.
    pub fn scale(&self) -> f64 {
        config::bar_scale(self.monitor.borrow().as_ref().map_or(1080, |m| m.geometry().height()))
    }

    /// The layer for hover hints over the card; see [`hint::float_below`].
    pub fn hints(&self) -> &gtk::Fixed {
        &self.hints
    }

    pub fn set_content(&self, content: &impl IsA<gtk::Widget>) {
        while let Some(child) = self.card.first_child() {
            self.card.remove(&child);
        }
        self.card.append(content);
    }

    pub fn show(self: &Rc<Self>, anchor: Anchor) {
        let monitor = self.monitor.borrow().clone();
        let s = self.scale();
        self.window.set_monitor(monitor.as_ref());
        self.top.set(config::FRAME_EDGE + config::BAR_HEIGHT * s + 6.0 * s);

        let inset = config::FRAME_EDGE + 12.0 * s;
        match anchor {
            Anchor::Right => {
                self.card.set_halign(gtk::Align::End);
                self.card.set_margin_start(0);
                self.card.set_margin_end(inset.round() as i32);
            }
            Anchor::Under(x) => {
                let screen = monitor.as_ref().map_or(1920, |m| m.geometry().width());
                // A measurement counts the widget's own margins, so clear the
                // last open's offset first.
                self.card.set_margin_start(0);
                self.card.set_margin_end(0);
                let width = self.card.measure(gtk::Orientation::Horizontal, -1).1;
                let left = (x - f64::from(width) / 2.0).clamp(inset, (f64::from(screen - width) - inset).max(inset));
                self.card.set_halign(gtk::Align::Start);
                self.card.set_margin_end(0);
                self.card.set_margin_start(left.round() as i32);
            }
        }

        self.open.set(true);
        self.window.present();
        self.animate(1.0);
    }

    pub fn hide(self: &Rc<Self>) {
        if self.open.replace(false) {
            self.animate(0.0);
        }
    }

    fn animate(self: &Rc<Self>, target: f64) {
        let popout = Rc::downgrade(self);
        anim::animate_to(&self.card, &self.openness, target, move |card, t| {
            let Some(popout) = popout.upgrade() else {
                return;
            };
            let eased = Easing::OutCubic.apply(t);
            card.set_opacity(eased);
            card.set_margin_top((popout.top.get() - DROP * (1.0 - eased)).round() as i32);
            if t <= 0.0 && !popout.open.get() {
                popout.window.set_visible(false);
            }
        });
    }
}

/// A section label: a short accent rule, the mark, and spaced-out caps.
pub fn eyebrow(s: f64, label: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (9.0 * s).round() as i32);
    let rule = gtk::DrawingArea::builder().content_width((20.0 * s).round() as i32).content_height((1.5 * s).ceil() as i32).valign(gtk::Align::Center).build();
    rule.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, theme::palette().accent);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    row.append(&rule);
    let seal = mark::mark(13.0 * s);
    seal.set_opacity(0.9);
    row.append(&seal);
    row.append(&TextStyle::new(9.5 * s, gtk::pango::Weight::Semibold, DIM).mono().tracking(2.6 * s).label(&label.to_uppercase()));
    row
}

/// The card surface, rounded and outlined, behind `content`.
pub fn card(content: &impl IsA<gtk::Widget>) -> gtk::Overlay {
    tinted_card(content, Rc::default())
}

/// [`card`], washed from its top-left corner with `tint` while it has one.
pub fn tinted_card(content: &impl IsA<gtk::Widget>, tint: Rc<Cell<Option<theme::Rgba>>>) -> gtk::Overlay {
    let surface = gtk::DrawingArea::new();
    surface.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, config::LAUNCHER_RADIUS);
        theme::set_source(cr, palette.base);
        let _ = cr.fill_preserve();
        if let Some(c) = tint.get() {
            let wash = gtk::cairo::RadialGradient::new(0.0, 0.0, 0.0, 0.0, 0.0, w.max(h));
            wash.add_color_stop_rgba(0.0, c.r, c.g, c.b, 0.22);
            wash.add_color_stop_rgba(1.0, c.r, c.g, c.b, 0.0);
            let _ = cr.set_source(&wash);
            let _ = cr.fill_preserve();
        }
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
    });
    let card = gtk::Overlay::new();
    card.set_child(Some(&surface));
    card.add_overlay(content);
    card.set_measure_overlay(content, true);
    card
}

/// A small spaced-out caps label heading a section of a card.
pub fn micro_label(s: f64, text: &str) -> gtk::Label {
    let label = TextStyle::new(9.0 * s, gtk::pango::Weight::Semibold, theme::SUBTLE).mono().tracking(1.2 * s).label(&text.to_uppercase());
    label.set_xalign(0.0);
    label
}

/// A hairline between sections of a card.
pub fn divider() -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_height(1).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, theme::HAIR);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line
}

/// Calls `f` with true as the pointer enters `widget` and false as it leaves.
pub fn on_hover(widget: &impl IsA<gtk::Widget>, f: impl Fn(bool) + 'static) {
    let f = Rc::new(f);
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter({
        let f = f.clone();
        move |_, _, _| f(true)
    });
    motion.connect_leave(move |_| f(false));
    widget.add_controller(motion);
}

/// Calls `f` when `widget` is clicked, and shows a pointer over it.
pub fn on_click(widget: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    widget.set_cursor_from_name(Some("pointer"));
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| f());
    widget.add_controller(click);
}

/// Lets a label fill what its row leaves it, ellipsized, rather than
/// widening the card to fit its whole text.
pub fn fill(label: &gtk::Label) {
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
}

/// Wires hover redraws and a click onto a drawn control.
pub fn clickable(area: &gtk::DrawingArea, hovered: &Rc<Cell<bool>>, enabled: bool, pressed: impl Fn() + 'static) {
    if !enabled {
        return;
    }
    area.set_cursor_from_name(Some("pointer"));
    let motion = gtk::EventControllerMotion::new();
    for entering in [true, false] {
        let (area, hovered) = (area.downgrade(), hovered.clone());
        let update = move || {
            hovered.set(entering);
            area.upgrade().into_iter().for_each(|a| a.queue_draw());
        };
        if entering {
            motion.connect_enter(move |_, _, _| update());
        } else {
            motion.connect_leave(move |_| update());
        }
    }
    area.add_controller(motion);
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| pressed());
    area.add_controller(click);
}

/// An on/off switch: a pill track with a knob that sits right when on.
pub fn switch(s: f64, on: bool, enabled: bool, pressed: impl Fn() + 'static) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width((30.0 * s).round() as i32).content_height((16.0 * s).round() as i32).valign(gtk::Align::Center).build();
    let hovered = Rc::new(Cell::new(false));
    area.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            let palette = theme::palette();
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
            theme::set_source(cr, if on { palette.accent.alpha(0.35) } else { theme::BRIGHT.alpha(0.06) });
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, if on { palette.accent_lit } else if hovered.get() { palette.edge } else { palette.outline });
            let _ = cr.stroke();
            let r = h / 2.0 - 3.0;
            let cx = if on { w - h / 2.0 } else { h / 2.0 };
            cr.arc(cx, h / 2.0, r, 0.0, std::f64::consts::TAU);
            theme::set_source(cr, if on { theme::CREAM } else { theme::ICON_DIM });
            let _ = cr.fill();
        }
    });
    if !enabled {
        area.set_opacity(0.5);
    }
    clickable(&area, &hovered, enabled, pressed);
    area
}
