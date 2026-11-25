//! Hover hints: a word or two, or a short note, on an outlined pill, shown the
//! moment the pointer arrives rather than after a tooltip's delay.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::pango;
use gtk4 as gtk;

use crate::popout::on_hover;
use crate::theme::{self, TextStyle, CREAM};

/// The hint itself, for placing in a layout.
pub fn pill(s: f64, text: &str) -> gtk::Overlay {
    let label = TextStyle::new(9.5 * s, pango::Weight::Semibold, CREAM).label(text);
    framed(s, &label, 8.0, 3.0)
}

/// A few sentences, wrapped to `width` logical px, on the same pill.
pub fn note(s: f64, text: &str, width: f64) -> gtk::Overlay {
    let label = TextStyle::new(10.0 * s, pango::Weight::Medium, CREAM).label(text);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_size_request((width * s).round() as i32, -1);
    label.set_max_width_chars(1);
    framed(s, &label, 11.0, 8.0)
}

/// Any label on the hint's outlined pill, padded by `pad_x` and `pad_y` logical
/// px; for text that has to read over anything, such as the overview's footer.
pub fn framed(s: f64, label: &gtk::Label, pad_x: f64, pad_y: f64) -> gtk::Overlay {
    let (x, y) = ((pad_x * s).round() as i32, (pad_y * s).round() as i32);
    label.set_margin_start(x);
    label.set_margin_end(x);
    label.set_margin_top(y);
    label.set_margin_bottom(y);
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 6.0 * s);
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.edge);
        let _ = cr.stroke();
    });
    let pill = gtk::Overlay::builder().valign(gtk::Align::Center).halign(gtk::Align::Center).can_target(false).build();
    pill.set_child(Some(&back));
    pill.add_overlay(label);
    pill.set_measure_overlay(label, true);
    pill
}

const LAYER_NAME: &str = "eclipse-hints";

/// A layer, laid over a whole window, that hints float on. It never takes
/// the pointer.
pub fn layer() -> gtk::Fixed {
    gtk::Fixed::builder().can_target(false).name(LAYER_NAME).build()
}

/// Shows `text` on `layer`, centred under `widget` with its top at `y`, while
/// the pointer is over `widget`.
pub fn float_under(widget: &impl IsA<gtk::Widget>, layer: &gtk::Fixed, s: f64, y: f64, text: &str) {
    let text = text.to_owned();
    let layer = layer.downgrade();
    float(widget, move |_| layer.upgrade(), s, Some(y), move || pill(s, &text));
}

/// Shows what `make` builds on `layer`, just below `widget` and centred on
/// it, while the pointer is over `widget`.
pub fn float_below(widget: &impl IsA<gtk::Widget>, layer: &gtk::Fixed, s: f64, make: impl Fn() -> gtk::Overlay + 'static) {
    let layer = layer.downgrade();
    float(widget, move |_| layer.upgrade(), s, None, make);
}

/// Shows `text` below `widget` (above, without room below) on its window's
/// hint layer, found when hovered: for widgets built before they know their
/// window. A window without a [`layer`] shows nothing.
pub fn float_label(widget: &impl IsA<gtk::Widget>, s: f64, text: &str) {
    let text = text.to_owned();
    float(widget, find_layer, s, None, move || pill(s, &text));
}

/// The hint layer in `widget`'s window.
fn find_layer(widget: &gtk::Widget) -> Option<gtk::Fixed> {
    let root = widget.root()?.upcast::<gtk::Widget>();
    let mut stack = vec![root];
    while let Some(w) = stack.pop() {
        if w.widget_name() == LAYER_NAME {
            return w.downcast().ok();
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
    None
}

/// `y` is the hint's top on the layer, or None for just below the widget
/// (or just above it, when below would run off the layer).
fn float(widget: &impl IsA<gtk::Widget>, layer: impl Fn(&gtk::Widget) -> Option<gtk::Fixed> + 'static, s: f64, y: Option<f64>, make: impl Fn() -> gtk::Overlay + 'static) {
    let widget_ref = widget.as_ref().downgrade();
    let shown: Rc<RefCell<Option<(gtk::Fixed, gtk::Overlay)>>> = Rc::default();
    // A widget removed while hovered never sees the pointer leave.
    widget.as_ref().connect_unmap({
        let shown = shown.clone();
        move |_| {
            if let Some((layer, old)) = shown.borrow_mut().take() {
                layer.remove(&old);
            }
        }
    });
    on_hover(widget, move |inside| {
        if let Some((layer, old)) = shown.borrow_mut().take() {
            layer.remove(&old);
        }
        let Some(widget) = widget_ref.upgrade().filter(|_| inside) else { return };
        let Some(layer) = layer(&widget) else { return };
        let Some(below) = widget.compute_point(&layer, &gtk::graphene::Point::new(widget.width() as f32 / 2.0, widget.height() as f32)) else { return };
        let hint = make();
        let width = f64::from(hint.measure(gtk::Orientation::Horizontal, -1).1);
        let height = f64::from(hint.measure(gtk::Orientation::Vertical, -1).1);
        let edge = 6.0 * s;
        let x = (f64::from(below.x()) - width / 2.0).clamp(edge, (f64::from(layer.width()) - width - edge).max(edge));
        let top = y.unwrap_or_else(|| {
            let under = f64::from(below.y()) + 6.0 * s;
            if under + height > f64::from(layer.height()) - edge && layer.height() > 0 {
                f64::from(below.y()) - f64::from(widget.height()) - 6.0 * s - height
            } else {
                under
            }
        });
        layer.put(&hint, x.round(), top.round());
        *shown.borrow_mut() = Some((layer, hint));
    });
}
