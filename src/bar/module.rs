//! The bar's module container: a fully rounded pill around its content that
//! lifts a soft overlay on hover and ripples outward from a press.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::anim::{self, Easing, Tween, EFFECTS_SLOW};
use crate::theme::{self, CREAM};

const HOVER_MS: f64 = 100.0;
const RIPPLE_GROW_MS: f64 = 500.0;
const RIPPLE_IN_MS: f64 = 60.0;
const RIPPLE_OUT_MS: f64 = 300.0;
const RIPPLE_PEAK: f64 = 0.10;
const HOVER_PEAK: f64 = 0.08;

pub struct ModuleSpec {
    /// Pill height, and width too when `square`.
    pub span: f64,
    pub pad_x: f64,
    /// Paint the pill surface; off for a bare mark that only reacts on hover.
    pub filled: bool,
    pub interactive: bool,
    pub square: bool,
}

#[derive(Default)]
struct Ripple {
    x: f64,
    y: f64,
    started_ms: Option<f64>,
    radius: f64,
    alpha: f64,
}

pub struct Module {
    pub widget: gtk::Overlay,
    tapped: Rc<RefCell<Option<Box<dyn Fn()>>>>,
}

impl Module {
    pub fn new(content: &impl IsA<gtk::Widget>, spec: ModuleSpec) -> Self {
        let base = gtk::DrawingArea::new();
        let width = if spec.square { spec.span.round() as i32 } else { -1 };
        base.set_size_request(width, spec.span.round() as i32);

        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Center);
        if !spec.square {
            content.set_margin_start(spec.pad_x.round() as i32);
            content.set_margin_end(spec.pad_x.round() as i32);
        }

        let widget = gtk::Overlay::builder().valign(gtk::Align::Center).build();
        widget.set_child(Some(&base));
        widget.add_overlay(content);
        widget.set_measure_overlay(content, true);

        let hover = Tween::shared(HOVER_MS, Easing::OutCubic);
        let ripple = Rc::new(RefCell::new(Ripple::default()));
        let filled = spec.filled;
        base.set_draw_func({
            let (hover, ripple) = (hover.clone(), ripple.clone());
            move |_, cr, w, h| {
                let (w, h) = (f64::from(w), f64::from(h));
                theme::rounded_rect(cr, 0.0, 0.0, w, h, w.min(h) / 2.0);
                cr.clip_preserve();
                if filled {
                    theme::set_source(cr, theme::palette().raised);
                    let _ = cr.fill_preserve();
                }
                let lift = hover.borrow().value * HOVER_PEAK;
                if lift > 0.0 {
                    theme::set_source(cr, CREAM.alpha(lift));
                    let _ = cr.fill();
                }
                let r = ripple.borrow();
                if r.alpha > 0.0 {
                    cr.new_path();
                    cr.arc(r.x, r.y, r.radius, 0.0, 2.0 * PI);
                    theme::set_source(cr, CREAM.alpha(r.alpha));
                    let _ = cr.fill();
                }
            }
        });
        theme::on_palette_change({
            let base = base.downgrade();
            move || {
                if let Some(base) = base.upgrade() {
                    base.queue_draw();
                }
            }
        });

        let tapped: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::default();
        if spec.interactive {
            widget.set_cursor_from_name(Some("pointer"));

            let motion = gtk::EventControllerMotion::new();
            let redraw = {
                let base = base.clone();
                move |_: &gtk::Widget, _| base.queue_draw()
            };
            motion.connect_enter({
                let (base, hover, redraw) = (base.clone(), hover.clone(), redraw.clone());
                move |_, _, _| anim::animate_to(&base, &hover, 1.0, redraw.clone())
            });
            motion.connect_leave({
                let (base, hover) = (base.clone(), hover.clone());
                move |_| anim::animate_to(&base, &hover, 0.0, redraw.clone())
            });
            widget.add_controller(motion);

            let click = gtk::GestureClick::new();
            click.connect_pressed({
                let (base, ripple) = (base.clone(), ripple.clone());
                move |_, _, x, y| start_ripple(&base, &ripple, x, y)
            });
            click.connect_released({
                let tapped = tapped.clone();
                move |_, _, _, _| {
                    if let Some(f) = tapped.borrow().as_ref() {
                        f();
                    }
                }
            });
            widget.add_controller(click);
        }

        Self { widget, tapped }
    }

    pub fn connect_tapped(&self, f: impl Fn() + 'static) {
        *self.tapped.borrow_mut() = Some(Box::new(f));
    }
}

/// Grows a circle from the press point past the pill's far corner while its
/// tint flashes in and fades out.
fn start_ripple(base: &gtk::DrawingArea, ripple: &Rc<RefCell<Ripple>>, x: f64, y: f64) {
    let already_running = ripple.borrow().started_ms.is_some();
    *ripple.borrow_mut() = Ripple { x, y, started_ms: None, radius: 0.0, alpha: 0.0 };
    // Restarting mid-ripple: the existing frame callback picks up the reset.
    if already_running {
        ripple.borrow_mut().started_ms = Some(-1.0);
        return;
    }
    ripple.borrow_mut().started_ms = Some(-1.0);
    let ripple = ripple.clone();
    base.add_tick_callback(move |base, clock| {
        let now = clock.frame_time() as f64 / 1000.0;
        let mut r = ripple.borrow_mut();
        let started = match r.started_ms {
            Some(t) if t >= 0.0 => t,
            _ => {
                r.started_ms = Some(now);
                now
            }
        };
        let elapsed = now - started;
        let reach = f64::from(base.width()).max(f64::from(base.height())) * 2.6;
        r.radius = reach / 2.0 * EFFECTS_SLOW.apply(elapsed / RIPPLE_GROW_MS);
        r.alpha = if elapsed < RIPPLE_IN_MS {
            RIPPLE_PEAK * elapsed / RIPPLE_IN_MS
        } else {
            RIPPLE_PEAK * (1.0 - Easing::OutCubic.apply((elapsed - RIPPLE_IN_MS) / RIPPLE_OUT_MS))
        };
        base.queue_draw();
        if elapsed >= RIPPLE_GROW_MS.max(RIPPLE_IN_MS + RIPPLE_OUT_MS) {
            *r = Ripple::default();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}
