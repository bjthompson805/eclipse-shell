//! A horizontal fader for the popouts: a thin track with an accent fill and a
//! resting tick, a percentage, and a leading glyph that can mute. Drag or
//! scroll the track to change it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, glib, pango};
use gtk4 as gtk;

use crate::popout::{on_click, on_hover};
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};

/// How long a fader ignores readings after it's moved, so a reading already
/// under way can't snap it back.
const SETTLE: Duration = Duration::from_millis(1500);
const SCROLL_STEP: f64 = 0.05;

pub struct Options {
    /// The glyph, and the one shown while muted.
    pub glyphs: (&'static str, &'static str),
    pub glyph_px: f64,
    /// Put the glyph at the fader's left; otherwise the caller places `glyph`.
    pub inline: bool,
    /// The lowest value it can be set to.
    pub floor: f64,
    /// The most often a drag reports a value. The last one always goes out.
    pub send_every: Duration,
}

pub struct Fader {
    s: f64,
    options: Options,
    value: Cell<f64>,
    muted: Cell<bool>,
    lit: Cell<bool>,
    dragging: Cell<bool>,
    /// When the user last changed it.
    touched: Cell<Option<Instant>>,
    sent: Cell<Option<Instant>>,
    pub widget: gtk::Box,
    track: gtk::DrawingArea,
    readout: gtk::Label,
    /// The leading glyph, inside `widget` or for the caller to place.
    pub glyph: gtk::Label,
    on_change: Box<dyn Fn(f64)>,
}

impl Fader {
    /// `on_change` gets each new value (0 to 1). With `on_mute`, clicking the
    /// glyph flips the mute and hands it the new state.
    pub fn new(s: f64, value: f64, muted: bool, options: Options, on_change: impl Fn(f64) + 'static, on_mute: Option<Box<dyn Fn(bool)>>) -> Rc<Self> {
        let px = |v: f64| (v * s).round() as i32;
        let widget = gtk::Box::builder().spacing(px(11.0)).height_request(px(28.0)).build();
        let glyph = theme::icon_glyph(options.glyphs.0, options.glyph_px, ICON_DIM);
        glyph.set_size_request(px(20.0), px(20.0));
        let track = gtk::DrawingArea::builder().hexpand(true).build();
        track.set_cursor_from_name(Some("pointer"));
        let readout = gtk::Label::new(None);
        readout.set_size_request(px(34.0), -1);
        readout.set_xalign(1.0);
        if options.inline {
            widget.append(&glyph);
        }
        widget.append(&track);
        widget.append(&readout);

        let fader = Rc::new(Self {
            s,
            value: Cell::new(value.clamp(options.floor, 1.0)),
            options,
            muted: Cell::new(muted),
            lit: Cell::new(false),
            dragging: Cell::new(false),
            touched: Cell::new(None),
            sent: Cell::new(None),
            widget,
            track,
            readout,
            glyph,
            on_change: Box::new(on_change),
        });

        let weak = Rc::downgrade(&fader);
        fader.track.set_draw_func(move |_, cr, w, h| {
            if let Some(f) = weak.upgrade() {
                f.draw(cr, f64::from(w), f64::from(h));
            }
        });

        if let Some(on_mute) = on_mute {
            let weak = Rc::downgrade(&fader);
            on_click(&fader.glyph, move || {
                if let Some(f) = weak.upgrade() {
                    f.muted.set(!f.muted.get());
                    f.touched.set(Some(Instant::now()));
                    on_mute(f.muted.get());
                    f.repaint();
                }
            });
        }

        let drag = gtk::GestureDrag::new();
        let start = Rc::new(Cell::new(0.0));
        drag.connect_drag_begin({
            let (weak, start) = (Rc::downgrade(&fader), start.clone());
            move |_, x, _| {
                start.set(x);
                if let Some(f) = weak.upgrade() {
                    f.dragging.set(true);
                    f.drag_to(x, false);
                }
            }
        });
        drag.connect_drag_update({
            let (weak, start) = (Rc::downgrade(&fader), start.clone());
            move |_, dx, _| weak.upgrade().into_iter().for_each(|f| f.drag_to(start.get() + dx, false))
        });
        drag.connect_drag_end({
            let weak = Rc::downgrade(&fader);
            move |_, dx, _| {
                if let Some(f) = weak.upgrade() {
                    f.drag_to(start.get() + dx, true);
                    f.dragging.set(false);
                }
            }
        });
        fader.track.add_controller(drag);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(&fader);
        scroll.connect_scroll(move |_, _, dy| {
            if let Some(f) = weak.upgrade().filter(|_| dy != 0.0) {
                f.set_value(f.value.get() + if dy < 0.0 { SCROLL_STEP } else { -SCROLL_STEP }, true);
            }
            glib::Propagation::Stop
        });
        fader.widget.add_controller(scroll);

        // Only the fader itself lights it, so hovering or clicking something
        // above it leaves the tick alone.
        let weak = Rc::downgrade(&fader);
        on_hover(&fader.widget, move |lit| weak.upgrade().into_iter().for_each(|f| f.set_lit(lit)));

        fader.repaint();
        fader
    }

    /// Takes a reading, unless the user is moving it.
    pub fn show(&self, value: f64, muted: bool) {
        let settling = self.touched.get().is_some_and(|t| t.elapsed() < SETTLE);
        if self.dragging.get() || settling {
            return;
        }
        let value = value.clamp(self.options.floor, 1.0);
        if (value - self.value.get()).abs() > 0.001 || muted != self.muted.get() {
            self.value.set(value);
            self.muted.set(muted);
            self.repaint();
        }
    }

    fn set_lit(&self, lit: bool) {
        self.lit.set(lit);
        self.repaint();
    }

    fn drag_to(&self, x: f64, last: bool) {
        let width = f64::from(self.track.width()).max(1.0);
        self.set_value(x / width, last);
    }

    /// Moves the fader; `force` reports the value even if one went out just now.
    fn set_value(&self, value: f64, force: bool) {
        let value = value.clamp(self.options.floor, 1.0);
        self.value.set(value);
        self.touched.set(Some(Instant::now()));
        if force || self.sent.get().is_none_or(|t| t.elapsed() >= self.options.send_every) {
            self.sent.set(Some(Instant::now()));
            (self.on_change)(value);
        }
        self.repaint();
    }

    fn repaint(&self) {
        let (muted, lit) = (self.muted.get(), self.lit.get());
        let text = if muted { "off".to_owned() } else { format!("{}%", (self.value.get() * 100.0).round()) };
        self.readout.set_text(&text);
        let color = if muted { FAINT } else if lit { CREAM } else { DIM };
        TextStyle::new(9.5 * self.s, pango::Weight::Semibold, color).apply(&self.readout);
        let name = if muted { self.options.glyphs.1 } else { self.options.glyphs.0 };
        self.glyph.set_text(name);
        let color = if muted { FAINT } else if lit { CREAM } else { ICON_DIM };
        self.glyph.set_attributes(theme::icon_glyph(name, self.options.glyph_px, color).attributes().as_ref());
        self.track.queue_draw();
    }

    fn draw(&self, cr: &cairo::Context, w: f64, h: f64) {
        let s = self.s;
        let palette = theme::palette();
        let (value, muted, lit) = (self.value.get(), self.muted.get(), self.lit.get());
        let thread = 3.0 * s;
        let y = (h - thread) / 2.0;
        theme::rounded_rect(cr, 0.0, y, w, thread, thread / 2.0);
        theme::set_source(cr, BRIGHT.alpha(0.08));
        let _ = cr.fill();

        let filled = w * value;
        if filled > 0.0 {
            let (from, to) = match () {
                _ if muted => (FAINT.alpha(0.5), FAINT),
                _ if lit => (palette.accent, palette.accent_lit),
                _ => (palette.accent.alpha(0.55), palette.accent.alpha(0.8)),
            };
            let gradient = cairo::LinearGradient::new(0.0, 0.0, filled, 0.0);
            add_stop(&gradient, 0.0, from);
            add_stop(&gradient, 1.0, to);
            theme::rounded_rect(cr, 0.0, y, filled.max(thread), thread, thread / 2.0);
            let _ = cr.set_source(&gradient);
            let _ = cr.fill();
        }

        // The tick marks where the fader rests, and gives way to the fill's glow on hover.
        if !lit {
            let (tw, th) = (2.5 * s, 11.0 * s);
            let x = (filled - tw / 2.0).clamp(0.0, (w - tw).max(0.0));
            theme::rounded_rect(cr, x, (h - th) / 2.0, tw, th, tw / 2.0);
            theme::set_source(cr, if muted { FAINT } else { SUBTLE });
            let _ = cr.fill();
        }
    }
}

fn add_stop(gradient: &cairo::LinearGradient, offset: f64, c: Rgba) {
    gradient.add_color_stop_rgba(offset, c.r, c.g, c.b, c.a);
}
