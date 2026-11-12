//! Small drawn controls for the wallpaper browser: buttons, segmented
//! choices, a search field and sliders, in the shell's look (cairo shapes,
//! Pango text, no CSS).

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::pango;
use gtk4 as gtk;

use crate::popout;
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, HAIR, ICON_DIM, SUBTLE};

/// A button: an optional glyph and a label. `primary` fills it with the
/// accent; a disabled one is dimmed and ignores clicks.
pub fn button(s: f64, glyph: Option<&str>, label: &str, primary: bool, enabled: bool, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let hovered = Rc::new(Cell::new(false));
    let back = gtk::DrawingArea::new();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 8.0 * s);
            let fill = match (primary, hovered.get() && enabled) {
                (true, false) => palette.accent.alpha(0.85),
                (true, true) => palette.accent,
                (false, false) => palette.raised,
                (false, true) => palette.raised.alpha(1.0),
            };
            theme::set_source(cr, fill);
            let _ = cr.fill_preserve();
            if !primary && hovered.get() && enabled {
                theme::set_source(cr, theme::ROW_HOVER);
                let _ = cr.fill_preserve();
            }
            cr.set_line_width(1.0);
            theme::set_source(cr, if primary { palette.accent_lit.alpha(0.6) } else if hovered.get() && enabled { theme::CHIP_EDGE } else { palette.outline });
            let _ = cr.stroke();
        }
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(7.0));
    row.set_halign(gtk::Align::Center);
    row.set_margin_start(px(14.0));
    row.set_margin_end(px(14.0));
    row.set_margin_top(px(7.0));
    row.set_margin_bottom(px(7.0));
    let ink = if primary { theme::BRIGHT } else { CREAM };
    if let Some(g) = glyph {
        row.append(&theme::icon_glyph(g, 15.0 * s, if primary { ink } else { ICON_DIM }));
    }
    row.append(&TextStyle::new(10.5 * s, pango::Weight::Semibold, ink).label(label));
    let button = gtk::Overlay::new();
    button.set_child(Some(&back));
    button.add_overlay(&row);
    button.set_measure_overlay(&row, true);
    if !enabled {
        button.set_opacity(0.4);
        return button;
    }
    popout::on_hover(&button, {
        let back = back.downgrade();
        move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
        }
    });
    popout::on_click(&button, pressed);
    button
}

/// A row of choices, the chosen one lit with an underline; `picked` gets
/// the index of a click on another.
pub fn segmented(s: f64, labels: &[&str], chosen: usize, picked: impl Fn(usize) + 'static) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(16.0));
    let picked = Rc::new(picked);
    for (i, label) in labels.iter().enumerate() {
        let on = i == chosen;
        let column = gtk::Box::new(gtk::Orientation::Vertical, px(4.0));
        column.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, if on { CREAM } else { DIM }).mono().tracking(1.2 * s).label(&label.to_uppercase()));
        let line = gtk::DrawingArea::builder().content_height(px(2.0)).build();
        line.set_draw_func(move |_, cr, w, h| {
            if on {
                theme::rounded_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), f64::from(h) / 2.0);
                theme::set_source(cr, theme::palette().accent);
                let _ = cr.fill();
            }
        });
        column.append(&line);
        if !on {
            let picked = picked.clone();
            popout::on_click(&column, move || picked(i));
        }
        row.append(&column);
    }
    row
}

/// Small pill choices for a tool's options (palette names and the like).
pub fn chips(s: f64, labels: &[&str], chosen: Option<usize>, picked: impl Fn(usize) + 'static) -> gtk::FlowBox {
    let px = |v: f64| (v * s).round() as i32;
    let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(px(6.0) as u32).row_spacing(px(6.0) as u32).max_children_per_line(8).homogeneous(false).build();
    let picked = Rc::new(picked);
    for (i, label) in labels.iter().enumerate() {
        let on = Some(i) == chosen;
        let text = TextStyle::new(9.5 * s, pango::Weight::Medium, if on { CREAM } else { SUBTLE }).mono().label(label);
        text.set_margin_start(px(9.0));
        text.set_margin_end(px(9.0));
        text.set_margin_top(px(3.0));
        text.set_margin_bottom(px(3.0));
        let back = gtk::DrawingArea::new();
        back.set_draw_func(move |_, cr, w, h| {
            let palette = theme::palette();
            theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, (f64::from(h) - 1.0) / 2.0);
            theme::set_source(cr, if on { palette.accent.alpha(0.2) } else { palette.raised });
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, if on { palette.accent.alpha(0.8) } else { palette.outline });
            let _ = cr.stroke();
        });
        let chip = gtk::Overlay::new();
        chip.set_child(Some(&back));
        chip.add_overlay(&text);
        chip.set_measure_overlay(&text, true);
        let picked = picked.clone();
        popout::on_click(&chip, move || picked(i));
        flow.append(&chip);
    }
    flow
}

/// A text field on a rounded recessed box, with a leading glyph.
pub fn field(s: f64, glyph: &str, placeholder: &str) -> (gtk::Overlay, gtk::Text) {
    let px = |v: f64| (v * s).round() as i32;
    let text = gtk::Text::builder().hexpand(true).placeholder_text(placeholder).valign(gtk::Align::Center).build();
    text.set_attributes(Some(&TextStyle::new(11.0 * s, pango::Weight::Medium, CREAM).attrs()));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    row.set_margin_start(px(11.0));
    row.set_margin_end(px(11.0));
    row.set_margin_top(px(6.0));
    row.set_margin_bottom(px(6.0));
    row.append(&theme::icon_glyph(glyph, 16.0 * s, ICON_DIM));
    row.append(&text);
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 9.0 * s);
        theme::set_source(cr, palette.recessed);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
    });
    let field = gtk::Overlay::new();
    field.set_child(Some(&back));
    field.add_overlay(&row);
    field.set_measure_overlay(&row, true);
    field.set_hexpand(true);
    (field, text)
}

/// A labelled slider over `min..=max` with a value readout; `changed` gets
/// each new value while dragging. An empty `name` leaves the label out.
pub fn slider(s: f64, name: &str, min: f64, max: f64, value: f64, format: fn(f64) -> String, changed: impl Fn(f64) + 'static) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
    if !name.is_empty() {
        let label = TextStyle::new(10.0 * s, pango::Weight::Medium, SUBTLE).label(name);
        label.set_width_chars(10);
        label.set_xalign(0.0);
        row.append(&label);
    }
    let readout = TextStyle::new(10.0 * s, pango::Weight::Medium, CREAM).mono().label(&format(value));
    readout.set_width_chars(5);
    readout.set_xalign(1.0);
    let value = Rc::new(Cell::new(value));
    let track = gtk::DrawingArea::builder().hexpand(true).content_height(px(20.0)).build();
    track.set_cursor_from_name(Some("pointer"));
    track.set_draw_func({
        let value = value.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            let line = 4.0 * s;
            let y = (h - line) / 2.0;
            let knob = 6.0 * s;
            let span = w - 2.0 * knob;
            theme::rounded_rect(cr, knob, y, span, line, line / 2.0);
            theme::set_source(cr, palette.raised);
            let _ = cr.fill();
            let at = knob + span * ((value.get() - min) / (max - min)).clamp(0.0, 1.0);
            // Filled from the middle for ranges around zero.
            let from = if min < 0.0 && max > 0.0 { knob + span * (-min / (max - min)) } else { knob };
            theme::rounded_rect(cr, from.min(at), y, (at - from).abs(), line, line / 2.0);
            theme::set_source(cr, palette.accent_lit);
            let _ = cr.fill();
            cr.arc(at, h / 2.0, knob, 0.0, 2.0 * std::f64::consts::PI);
            theme::set_source(cr, CREAM);
            let _ = cr.fill();
        }
    });
    let changed = Rc::new(changed);
    let set = {
        let (value, track, readout, changed) = (value.clone(), track.downgrade(), readout.clone(), changed.clone());
        Rc::new(move |x: f64| {
            let Some(track) = track.upgrade() else { return };
            let knob = 6.0 * s;
            let span = f64::from(track.width()) - 2.0 * knob;
            let v = (min + (max - min) * ((x - knob) / span.max(1.0)).clamp(0.0, 1.0)).round();
            if v != value.get() {
                value.set(v);
                readout.set_text(&format(v));
                track.queue_draw();
                changed(v);
            }
        })
    };
    let drag = gtk::GestureDrag::new();
    drag.connect_drag_begin({
        let set = set.clone();
        move |_, x, _| set(x)
    });
    drag.connect_drag_update({
        let set = set.clone();
        move |g, dx, _| {
            if let Some((x, _)) = g.start_point() {
                set(x + dx);
            }
        }
    });
    track.add_controller(drag);
    row.append(&track);
    row.append(&readout);
    row
}

/// A hairline-boxed note, for empty states and errors.
pub fn note(s: f64, glyph: &str, text: &str) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, (8.0 * s).round() as i32);
    column.set_valign(gtk::Align::Center);
    column.set_halign(gtk::Align::Center);
    column.set_vexpand(true);
    column.append(&theme::icon_glyph(glyph, 28.0 * s, FAINT));
    let label = TextStyle::new(11.0 * s, pango::Weight::Medium, DIM).label(text);
    label.set_wrap(true);
    label.set_justify(gtk::Justification::Center);
    label.set_max_width_chars(40);
    column.append(&label);
    column
}

/// A vertical hairline.
pub fn rule_v() -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_width(1).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, HAIR);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line
}
