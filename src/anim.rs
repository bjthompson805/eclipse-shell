//! Eased tweens driven by GTK's frame clock.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

#[derive(Clone, Copy, Debug)]
pub enum Easing {
    OutCubic,
    InOutSine,
    /// CSS-style cubic-bezier(x1, y1, x2, y2).
    Bezier(f64, f64, f64, f64),
}

/// Fast settle with a long, soft tail; used for open/close and resizes.
pub const MORPH: Easing = Easing::Bezier(0.16, 1.0, 0.3, 1.0);
pub const EFFECTS_SLOW: Easing = Easing::Bezier(0.34, 0.88, 0.34, 1.0);

impl Easing {
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::OutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::InOutSine => -((std::f64::consts::PI * t).cos() - 1.0) / 2.0,
            Easing::Bezier(x1, y1, x2, y2) => bezier(x1, y1, x2, y2, t),
        }
    }
}

/// Solves the curve for `x` by bisection, then returns its `y`.
fn bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let coord = |a: f64, b: f64, t: f64| {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    };
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..32 {
        let mid = (lo + hi) / 2.0;
        if coord(x1, x2, mid) < x { lo = mid } else { hi = mid }
    }
    coord(y1, y2, (lo + hi) / 2.0)
}

pub struct Tween {
    pub value: f64,
    from: f64,
    target: f64,
    duration_ms: f64,
    easing: Easing,
    started_at: Option<i64>,
    running: bool,
    initialised: bool,
}

impl Tween {
    pub fn new(duration_ms: f64, easing: Easing) -> Self {
        Self { value: 0.0, from: 0.0, target: 0.0, duration_ms, easing, started_at: None, running: false, initialised: false }
    }

    pub fn shared(duration_ms: f64, easing: Easing) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self::new(duration_ms, easing)))
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    /// Moves straight to `value` without animating.
    pub fn jump(&mut self, value: f64) {
        self.initialised = true;
        self.value = value;
        self.from = value;
        self.target = value;
        self.started_at = None;
    }

    /// Retargets the tween. Returns true when a frame callback needs starting;
    /// the first value ever set is applied immediately.
    fn retarget(&mut self, target: f64) -> bool {
        if !self.initialised {
            self.jump(target);
            return false;
        }
        if target == self.target {
            return false;
        }
        self.from = self.value;
        self.target = target;
        self.started_at = None;
        let start = !self.running;
        self.running = true;
        start
    }

    /// Advances to `frame_time` (µs). Returns true while still moving.
    fn step(&mut self, frame_time: i64) -> bool {
        if !self.running {
            return false;
        }
        let started = *self.started_at.get_or_insert(frame_time);
        let t = if self.duration_ms <= 0.0 { 1.0 } else { (frame_time - started) as f64 / 1000.0 / self.duration_ms };
        self.value = self.from + (self.target - self.from) * self.easing.apply(t);
        self.running = t < 1.0;
        if !self.running {
            self.value = self.target;
        }
        self.running
    }
}

/// Animates `tween` towards `target` on `widget`'s frame clock, calling
/// `apply` with each new value.
pub fn animate_to<W, F>(widget: &W, tween: &Rc<RefCell<Tween>>, target: f64, apply: F)
where
    W: IsA<gtk::Widget>,
    F: Fn(&gtk::Widget, f64) + 'static,
{
    let start = tween.borrow_mut().retarget(target);
    apply(widget.upcast_ref(), tween.borrow().value);
    if !start {
        return;
    }
    let tween = tween.clone();
    widget.add_tick_callback(move |widget, clock| {
        let running = tween.borrow_mut().step(clock.frame_time());
        apply(widget.upcast_ref(), tween.borrow().value);
        if running { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
    });
}

/// Calls `frame` every frame while `widget` is mapped, with the frame time in
/// milliseconds. For continuous effects (breathing, drifting) only.
pub fn while_mapped<W, F>(widget: &W, frame: F)
where
    W: IsA<gtk::Widget>,
    F: Fn(&gtk::Widget, f64) + 'static,
{
    let frame = Rc::new(frame);
    widget.connect_map(move |widget| {
        let frame = frame.clone();
        widget.add_tick_callback(move |widget, clock| {
            if !widget.is_mapped() {
                return glib::ControlFlow::Break;
            }
            frame(widget.upcast_ref(), clock.frame_time() as f64 / 1000.0);
            glib::ControlFlow::Continue
        });
    });
}
