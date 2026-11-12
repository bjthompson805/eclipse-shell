//! Hover hints: a word or two on an outlined pill, shown the moment the
//! pointer arrives rather than after a tooltip's delay.

use gtk::prelude::*;
use gtk::pango;
use gtk4 as gtk;

use crate::popout::on_hover;
use crate::theme::{self, TextStyle, CREAM};

/// The hint itself, for placing in a layout.
pub fn pill(s: f64, text: &str) -> gtk::Overlay {
    let label = TextStyle::new(9.5 * s, pango::Weight::Semibold, CREAM).label(text);
    let (x, y) = ((8.0 * s).round() as i32, (3.0 * s).round() as i32);
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
    pill.add_overlay(&label);
    pill.set_measure_overlay(&label, true);
    pill
}

/// A layer, laid over a whole window, that hints float on. It never takes
/// the pointer.
pub fn layer() -> gtk::Fixed {
    gtk::Fixed::builder().can_target(false).build()
}

/// Shows `text` on `layer`, centred under `widget` with its top at `y`, while
/// the pointer is over `widget`.
pub fn float_under(widget: &impl IsA<gtk::Widget>, layer: &gtk::Fixed, s: f64, y: f64, text: &str) {
    let (widget_ref, layer, text) = (widget.as_ref().downgrade(), layer.downgrade(), text.to_owned());
    let shown: std::rc::Rc<std::cell::RefCell<Option<gtk::Overlay>>> = Default::default();
    // A widget removed while hovered never sees the pointer leave.
    widget.as_ref().connect_unmap({
        let (layer, shown) = (layer.clone(), shown.clone());
        move |_| {
            if let (Some(layer), Some(old)) = (layer.upgrade(), shown.borrow_mut().take()) {
                layer.remove(&old);
            }
        }
    });
    on_hover(widget, move |inside| {
        let Some(layer) = layer.upgrade() else { return };
        if let Some(old) = shown.borrow_mut().take() {
            layer.remove(&old);
        }
        let Some(widget) = widget_ref.upgrade().filter(|_| inside) else { return };
        let Some(centre) = widget.compute_point(&layer, &gtk::graphene::Point::new(widget.width() as f32 / 2.0, 0.0)) else { return };
        let hint = pill(s, &text);
        let width = f64::from(hint.measure(gtk::Orientation::Horizontal, -1).1);
        let edge = 6.0 * s;
        let x = (f64::from(centre.x()) - width / 2.0).clamp(edge, (f64::from(layer.width()) - width - edge).max(edge));
        layer.put(&hint, x.round(), y.round());
        *shown.borrow_mut() = Some(hint);
    });
}
