//! The screenshot toolbar under the selection, and the capture-hotkey panel
//! its gear opens. Both only draw what [`View`] says and report clicks as
//! [`Action`]s; the screenshot owns the state.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, glib, pango};
use gtk4 as gtk;

use super::icons;
use crate::popout;
use crate::theme::{self, Rgba, TextStyle, SUBTLE};

pub const TOOLS: &[(&str, &str)] = &[
    ("select", "Select and move"),
    ("rect", "Rectangle"),
    ("ellipse", "Ellipse"),
    ("line", "Line"),
    ("arrow", "Arrow"),
    ("pen", "Pen"),
    ("marker", "Highlighter"),
    ("text", "Text"),
    ("blur", "Blur"),
    ("pixelate", "Pixelate"),
    ("magnify", "Magnifier"),
    ("counter", "Numbered step"),
];

pub const SWATCHES: &[(Rgba, &str)] = &[
    (Rgba::rgb8(0xe2, 0x34, 0x2a), "Vermilion"),
    (Rgba::rgb8(0xff, 0xff, 0xff), "White"),
    (Rgba::rgb8(0x1a, 0x1a, 0x1a), "Black"),
    (Rgba::rgb8(0xe2, 0x3b, 0x3b), "Red"),
    (Rgba::rgb8(0xf2, 0xc1, 0x4e), "Yellow"),
    (Rgba::rgb8(0x5b, 0xbf, 0x73), "Green"),
    (Rgba::rgb8(0x4f, 0x8f, 0xe0), "Blue"),
];

/// Stroke widths: the width, its dot's size, and its name.
const WIDTHS: &[(f64, f64, &str)] = &[(2.0, 5.0, "Thin stroke"), (4.0, 9.0, "Medium stroke"), (7.0, 13.0, "Thick stroke")];

#[derive(Clone)]
pub enum Action {
    Tool(&'static str),
    Color(Rgba),
    Width(f64),
    Rough,
    Undo,
    Redo,
    Copy,
    Save,
    Upload,
    Settings,
    /// The hotkey panel's Record button.
    Listen,
}

/// What the toolbar shows.
#[derive(Clone)]
pub struct View {
    pub tool: &'static str,
    pub color: Rgba,
    pub width: f64,
    pub rough: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub settings_open: bool,
    pub hotkey: String,
    pub listening: bool,
}

pub struct Toolbar {
    pub widget: gtk::Overlay,
    /// The hotkey panel, placed above the gear.
    pub panel: gtk::Overlay,
    /// The hover tooltip; it goes in the Fixed last, over the rest.
    pub tip: gtk::Overlay,
    gear: gtk::DrawingArea,
    view: Rc<RefCell<View>>,
    redraw: Vec<gtk::DrawingArea>,
    hotkey: gtk::Label,
    record: gtk::Label,
}

const GLASS: Rgba = Rgba::rgba8(22, 17, 11, 0.94);

fn glass(content: &impl IsA<gtk::Widget>, radius: f64) -> gtk::Overlay {
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius);
        theme::set_source(cr, GLASS);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, Rgba::rgba8(243, 237, 225, 0.14));
        let _ = cr.stroke();
    });
    let card = gtk::Overlay::new();
    card.set_child(Some(&back));
    card.add_overlay(content);
    card.set_measure_overlay(content, true);
    card
}

fn separator() -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_width(1).content_height(20).valign(gtk::Align::Center).margin_start(3).margin_end(3).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, Rgba::rgba8(243, 237, 225, 0.14));
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line
}

/// ryoshot's hover label: shown the moment a control is hovered, centred
/// under it (over it when that would run off the screen), with its keys in a
/// chip. It lives in the overlay's Fixed, above the toolbar.
struct Tip {
    widget: gtk::Overlay,
    text: gtk::Label,
    keys: gtk::Label,
    chip: gtk::Overlay,
    row: gtk::Box,
    fixed: gtk::Fixed,
}

impl Tip {
    fn new(fixed: &gtk::Fixed) -> Rc<Self> {
        let back = gtk::DrawingArea::new();
        back.set_draw_func(|_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            // A soft drop, then the card.
            theme::rounded_rect(cr, 0.0, 4.0, w, h - 4.0, 8.0);
            theme::set_source(cr, Rgba::rgba8(0, 0, 0, 0.35));
            let _ = cr.fill();
            theme::rounded_rect(cr, 0.5, 2.5, w - 1.0, h - 5.0, 7.0);
            theme::set_source(cr, Rgba::rgba8(22, 17, 11, 0.97));
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, Rgba::rgba8(243, 237, 225, 0.14));
            let _ = cr.stroke();
        });
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_start(10);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        let text = TextStyle::new(12.0, pango::Weight::Medium, Rgba::rgb8(0xf5, 0xef, 0xe4)).label("");
        text.set_valign(gtk::Align::Center);
        row.append(&text);
        let keys = TextStyle::new(10.0, pango::Weight::Medium, Rgba::rgb8(0x8f, 0x83, 0x78)).mono().label("");
        keys.set_margin_start(5);
        keys.set_margin_end(5);
        let chip_back = gtk::DrawingArea::new();
        chip_back.set_draw_func(|_, cr, w, h| {
            theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 4.0);
            theme::set_source(cr, Rgba::rgba8(255, 255, 255, 0.06));
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, Rgba::rgba8(243, 237, 225, 0.12));
            let _ = cr.stroke();
        });
        let chip = gtk::Overlay::new();
        chip.set_child(Some(&chip_back));
        chip.add_overlay(&keys);
        chip.set_measure_overlay(&keys, true);
        chip.set_size_request(-1, 18);
        chip.set_valign(gtk::Align::Center);
        row.append(&chip);
        let widget = gtk::Overlay::new();
        widget.set_child(Some(&back));
        widget.add_overlay(&row);
        widget.set_measure_overlay(&row, true);
        widget.set_can_target(false);
        widget.set_visible(false);
        Rc::new(Self { widget, text, keys, chip, row, fixed: fixed.clone() })
    }

    /// Shows `text` (and `keys`) for `over`, at once.
    fn show(&self, over: &gtk::Widget, text: &str, keys: &str) {
        if text.is_empty() || !over.is_visible() {
            return;
        }
        self.text.set_text(text);
        self.keys.set_text(keys);
        self.chip.set_visible(!keys.is_empty());
        self.row.set_margin_end(if keys.is_empty() { 10 } else { 6 });
        let (_, nat) = self.widget.preferred_size();
        let (tw, th) = (f64::from(nat.width()), f64::from(nat.height()));
        let (Some(top), Some(bottom)) = (
            over.compute_point(&self.fixed, &gtk::graphene::Point::new(0.0, 0.0)),
            over.compute_point(&self.fixed, &gtk::graphene::Point::new(over.width() as f32, over.height() as f32)),
        ) else {
            return;
        };
        let (fw, fh) = (f64::from(self.fixed.width()), f64::from(self.fixed.height()));
        let gap = 10.0 - 2.0;
        let below = f64::from(bottom.y()) + gap;
        let y = if below + th + 8.0 <= fh { below } else { f64::from(top.y()) - th - gap };
        let x = ((f64::from(top.x()) + f64::from(bottom.x())) / 2.0 - tw / 2.0).clamp(8.0, (fw - tw - 8.0).max(8.0));
        self.fixed.move_(&self.widget, x.round(), y.round());
        self.widget.set_opacity(0.0);
        self.widget.set_visible(true);
        // A 90ms fade in.
        let widget = self.widget.downgrade();
        let started = std::time::Instant::now();
        self.widget.add_tick_callback(move |_, _| {
            let t = (started.elapsed().as_secs_f64() / 0.09).min(1.0);
            widget.upgrade().into_iter().for_each(|w| w.set_opacity(1.0 - (1.0 - t).powi(3)));
            if t >= 1.0 { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
        });
    }

    fn hide(&self) {
        self.widget.set_visible(false);
    }
}

impl Toolbar {
    /// The toolbar, its panel, and the tooltip they share, which goes in `fixed`.
    pub fn new(view: View, act: Rc<dyn Fn(Action)>, fixed: &gtk::Fixed) -> Rc<Self> {
        let view = Rc::new(RefCell::new(view));
        let tip = Tip::new(fixed);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        row.set_margin_start(6);
        row.set_margin_end(6);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        let mut redraw = Vec::new();

        // A 32px square that draws `paint` over a hover tint and calls `pressed`.
        let square = |size: i32, (text, keys): (String, &'static str), paint: Box<dyn Fn(&cairo::Context, f64, bool)>, pressed: Box<dyn Fn()>| {
            let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).build();
            let hovered = Rc::new(Cell::new(false));
            area.set_draw_func({
                let hovered = hovered.clone();
                move |_, cr, w, _| paint(cr, f64::from(w), hovered.get())
            });
            area.set_cursor_from_name(Some("pointer"));
            popout::on_hover(&area, {
                let (hovered, area, tip) = (hovered, area.downgrade(), tip.clone());
                move |inside| {
                    hovered.set(inside);
                    let Some(area) = area.upgrade() else { return };
                    area.queue_draw();
                    if inside { tip.show(area.upcast_ref(), &text, keys) } else { tip.hide() }
                }
            });
            popout::on_click(&area, pressed);
            area
        };
        // An icon button: `on` lights it, `dim` greys it and ignores clicks.
        let icon_button = |name: &'static str, (text, keys): (&str, &'static str), on: Rc<dyn Fn(&View) -> bool>, dim: Rc<dyn Fn(&View) -> bool>, action: Action| {
            let paint = {
                let (view, on, dim) = (view.clone(), on.clone(), dim.clone());
                Box::new(move |cr: &cairo::Context, size: f64, hovered: bool| {
                    let v = view.borrow();
                    let (on, dim) = (on(&v), dim(&v));
                    if on || (hovered && !dim) {
                        theme::rounded_rect(cr, 0.0, 0.0, size, size, 7.0);
                        theme::set_source(cr, if on { theme::palette().accent } else { Rgba::rgba8(255, 255, 255, 0.06) });
                        let _ = cr.fill();
                    }
                    let tint = if on { Rgba::rgb8(0xff, 0xff, 0xff) } else if dim { Rgba { r: 0.77, g: 0.80, b: 0.85, a: 0.35 } } else { SUBTLE };
                    theme::set_source(cr, tint);
                    icons::draw(cr, name, (size - 18.0) / 2.0, (size - 18.0) / 2.0, 18.0);
                }) as Box<dyn Fn(&cairo::Context, f64, bool)>
            };
            let pressed = {
                let (view, act) = (view.clone(), act.clone());
                Box::new(move || {
                    if !dim(&view.borrow()) {
                        act(action.clone());
                    }
                }) as Box<dyn Fn()>
            };
            square(32, (text.to_owned(), keys), paint, pressed)
        };
        let never: Rc<dyn Fn(&View) -> bool> = Rc::new(|_| false);

        for &(name, tip) in TOOLS {
            let b = icon_button(name, (tip, ""), Rc::new(move |v: &View| v.tool == name), never.clone(), Action::Tool(name));
            row.append(&b);
            redraw.push(b);
        }
        row.append(&separator());
        for &(color, name) in SWATCHES {
            let paint = {
                let view = view.clone();
                Box::new(move |cr: &cairo::Context, size: f64, hovered: bool| {
                    let chosen = view.borrow().color == color;
                    let r = if hovered { 10.0 } else { 9.0 };
                    cr.arc(size / 2.0, size / 2.0, r - 0.5, 0.0, std::f64::consts::TAU);
                    theme::set_source(cr, color);
                    let _ = cr.fill_preserve();
                    cr.set_line_width(if chosen { 2.0 } else { 1.0 });
                    theme::set_source(cr, if chosen { Rgba::rgb8(0xff, 0xff, 0xff) } else { Rgba::rgba8(255, 255, 255, 0.18) });
                    let _ = cr.stroke();
                }) as Box<dyn Fn(&cairo::Context, f64, bool)>
            };
            let act = act.clone();
            let b = square(22, (name.to_owned(), ""), paint, Box::new(move || act(Action::Color(color))));
            row.append(&b);
            redraw.push(b);
        }
        row.append(&separator());
        for &(width, dot, tip) in WIDTHS {
            let paint = {
                let view = view.clone();
                Box::new(move |cr: &cairo::Context, size: f64, hovered: bool| {
                    let chosen = view.borrow().width == width;
                    if chosen || hovered {
                        theme::rounded_rect(cr, 0.0, 0.0, size, size, 6.0);
                        theme::set_source(cr, if chosen { theme::palette().accent } else { Rgba::rgba8(255, 255, 255, 0.06) });
                        let _ = cr.fill();
                    }
                    cr.arc(size / 2.0, size / 2.0, dot / 2.0, 0.0, std::f64::consts::TAU);
                    theme::set_source(cr, if chosen { Rgba::rgb8(0xff, 0xff, 0xff) } else { SUBTLE });
                    let _ = cr.fill();
                }) as Box<dyn Fn(&cairo::Context, f64, bool)>
            };
            let act = act.clone();
            let b = square(26, (tip.to_owned(), ""), paint, Box::new(move || act(Action::Width(width))));
            row.append(&b);
            redraw.push(b);
        }
        row.append(&separator());
        let b = icon_button("sketch", ("Hand-drawn style", ""), Rc::new(|v: &View| v.rough), never.clone(), Action::Rough);
        row.append(&b);
        redraw.push(b);
        row.append(&separator());
        let b = icon_button("undo", ("Undo", "Ctrl Z"), never.clone(), Rc::new(|v: &View| !v.can_undo), Action::Undo);
        row.append(&b);
        redraw.push(b);
        let b = icon_button("redo", ("Redo", "Ctrl Y"), never.clone(), Rc::new(|v: &View| !v.can_redo), Action::Redo);
        row.append(&b);
        redraw.push(b);
        row.append(&separator());
        for (name, tip, action) in [("copy", ("Copy to clipboard", "Ctrl C"), Action::Copy), ("save", ("Save to file", ""), Action::Save), ("upload", ("Upload and copy a public link (expires in 3 days)", ""), Action::Upload)] {
            row.append(&icon_button(name, tip, never.clone(), never.clone(), action));
        }
        row.append(&separator());
        let gear = icon_button("gear", ("Capture hotkey", ""), Rc::new(|v: &View| v.settings_open), never.clone(), Action::Settings);
        row.append(&gear);
        redraw.push(gear.clone());
        let widget = glass(&row, 10.0);

        // The hotkey panel: the bind, and a button that records a new one.
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        line.set_margin_start(10);
        line.set_margin_end(10);
        line.set_margin_top(8);
        line.set_margin_bottom(8);
        let hotkey = TextStyle::new(13.0, pango::Weight::Medium, SUBTLE).mono().label("");
        hotkey.set_valign(gtk::Align::Center);
        line.append(&hotkey);
        let record = TextStyle::new(13.0, pango::Weight::Medium, SUBTLE).mono().label("Record");
        record.set_margin_start(12);
        record.set_margin_end(12);
        record.set_margin_top(5);
        record.set_margin_bottom(5);
        let record_back = gtk::DrawingArea::new();
        record_back.set_draw_func({
            let view = view.clone();
            move |_, cr, w, h| {
                let listening = view.borrow().listening;
                theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 6.0);
                theme::set_source(cr, if listening { theme::palette().accent } else { Rgba::rgba8(255, 255, 255, 0.06) });
                let _ = cr.fill_preserve();
                cr.set_line_width(1.0);
                theme::set_source(cr, if listening { theme::palette().accent } else { Rgba::rgba8(243, 237, 225, 0.14) });
                let _ = cr.stroke();
            }
        });
        let button = gtk::Overlay::new();
        button.set_child(Some(&record_back));
        button.add_overlay(&record);
        button.set_measure_overlay(&record, true);
        button.set_cursor_from_name(Some("pointer"));
        popout::on_hover(&button, {
            let (tip, view, b) = (tip.clone(), view.clone(), button.downgrade());
            move |inside| match b.upgrade() {
                Some(b) if inside => {
                    let text = if view.borrow().listening { "Press Esc to cancel" } else { "Record a new capture hotkey" };
                    tip.show(b.upcast_ref(), text, "");
                }
                _ => tip.hide(),
            }
        });
        popout::on_click(&button, {
            let act = act.clone();
            move || act(Action::Listen)
        });
        line.append(&button);
        redraw.push(record_back);
        let panel = glass(&line, 10.0);
        panel.set_visible(false);

        let tip_widget = tip.widget.clone();
        Rc::new(Self { widget, panel, tip: tip_widget, gear, view, redraw, hotkey, record })
    }

    pub fn update(&self, view: View) {
        self.hotkey.set_text(&view.hotkey);
        self.record.set_text(if view.listening { "Press a key…" } else { "Record" });
        self.panel.set_visible(view.settings_open && self.widget.is_visible());
        *self.view.borrow_mut() = view;
        self.redraw.iter().for_each(|a| a.queue_draw());
    }

    /// The gear's centre, in the toolbar's coordinates.
    pub fn gear_x(&self) -> f64 {
        self.gear.compute_point(&self.widget, &gtk::graphene::Point::new(16.0, 0.0)).map_or(0.0, |p| f64::from(p.x()))
    }
}
