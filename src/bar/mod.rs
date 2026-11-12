//! Per-monitor bar: a frame around the whole screen whose top edge thickens
//! into a band carrying the modules.
//!
//! Two layer surfaces per monitor: a full-screen one that draws the frame and
//! the bar (taking input only on the band), and an invisible strip that
//! reserves the band's height so tiled windows sit below it.

mod clock;
mod module;
mod title;
mod workspaces;

pub use clock::every_second;

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config;
use crate::hyprland::State;
use crate::mark;
use crate::theme;
use module::{Module, ModuleSpec};
use title::Title;
use workspaces::Workspaces;

pub struct Bar {
    pub connector: Option<String>,
    windows: [gtk::ApplicationWindow; 2],
    workspaces: Rc<Workspaces>,
    title: Rc<Title>,
}

impl Bar {
    pub fn new(app: &gtk::Application, monitor: &gdk::Monitor, on_mark: Rc<dyn Fn()>) -> Self {
        let s = config::bar_scale(monitor.geometry().height());
        let band = config::BAR_HEIGHT * s;
        let top = config::FRAME_EDGE + band;

        let reserve = layer_window(app, monitor, "eclipse-shell-reserve");
        reserve.set_layer(Layer::Top);
        for edge in [Edge::Top, Edge::Left, Edge::Right] {
            reserve.set_anchor(edge, true);
        }
        reserve.set_exclusive_zone(top.round() as i32);
        reserve.set_default_size(0, 0);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_size_request(-1, top.round() as i32);
        reserve.set_child(Some(&spacer));
        // It only reserves space; clicks fall through to the frame above it.
        reserve.connect_realize(|w| set_input_rect(w, 0, 0));

        let frame = layer_window(app, monitor, "eclipse-shell");
        frame.set_layer(Layer::Top);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            frame.set_anchor(edge, true);
        }
        // Span the whole output rather than the space other bars leave.
        frame.set_exclusive_zone(-1);

        let canvas = gtk::DrawingArea::new();
        canvas.set_draw_func(move |_, cr, w, h| draw_frame(cr, f64::from(w), f64::from(h), top));
        canvas.connect_resize({
            let frame = frame.clone();
            move |_, w, _| set_input_rect(&frame, w, top.ceil() as i32)
        });
        theme::on_palette_change({
            let canvas = canvas.downgrade();
            move || {
                if let Some(canvas) = canvas.upgrade() {
                    canvas.queue_draw();
                }
            }
        });

        let workspaces = Workspaces::new(s);
        let title = Title::new(s);
        let content = band_content(s, band, &workspaces, &title, on_mark);
        content.set_size_request(-1, top.round() as i32);

        let layers = gtk::Overlay::new();
        layers.set_child(Some(&canvas));
        layers.add_overlay(&content);
        frame.set_child(Some(&layers));

        reserve.present();
        frame.present();
        Self { connector: monitor.connector().map(String::from), windows: [reserve, frame], workspaces, title }
    }

    pub fn update(&self, state: &State) {
        self.workspaces.update(state, self.connector.as_deref());
        self.title.update(state.focused.as_ref());
    }

    pub fn destroy(&self) {
        for window in &self.windows {
            window.destroy();
        }
    }
}

fn layer_window(app: &gtk::Application, monitor: &gdk::Monitor, namespace: &str) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
    window.init_layer_shell();
    window.set_namespace(Some(namespace));
    window.set_monitor(Some(monitor));
    window.set_keyboard_mode(KeyboardMode::None);
    window
}

/// Limits pointer input to a strip along the top; the rest of the surface
/// lets clicks through to the windows underneath.
fn set_input_rect(window: &impl IsA<gtk::Native>, width: i32, height: i32) {
    if let Some(surface) = window.surface() {
        surface.set_input_region(Some(&cairo::Region::create_rectangle(&cairo::RectangleInt::new(0, 0, width, height))));
    }
}

/// The frame: a solid surface around the screen with a rounded window-shaped
/// hole, an edge line around the hole, and a shadow falling into it.
fn draw_frame(cr: &cairo::Context, w: f64, h: f64, top: f64) {
    let palette = theme::palette();
    let edge = config::FRAME_EDGE;
    let (hx, hy, hw, hh) = (edge, top, w - 2.0 * edge, h - top - edge);
    let hole = |cr: &cairo::Context, inset: f64| {
        theme::rounded_rect(cr, hx + inset, hy + inset, hw - 2.0 * inset, hh - 2.0 * inset, (config::FRAME_RADIUS - inset).max(0.0));
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

/// The band: mark, workspaces and title on the left, the clock centred, and
/// the power button on the right.
fn band_content(s: f64, band: f64, workspaces: &Rc<Workspaces>, title: &Rc<Title>, on_mark: Rc<dyn Fn()>) -> gtk::CenterBox {
    let span = (band * 0.76).round();
    let edge_margin = (24.0 * s).round() as i32;
    let pill = |pad_x: f64| ModuleSpec { span, pad_x, filled: true, interactive: true, square: false };

    let mark = Module::new(&mark::mark(11.0 * s), ModuleSpec { filled: false, square: true, ..pill(0.0) });
    mark.connect_tapped(move || on_mark());
    let spaces = Module::new(&workspaces.widget, ModuleSpec { interactive: false, ..pill(10.0 * s) });

    let left = gtk::Box::builder().spacing((8.0 * s).round() as i32).valign(gtk::Align::Center).margin_start(edge_margin).build();
    left.append(&mark.widget);
    left.append(&spaces.widget);
    left.append(&title.widget);

    let clock = Module::new(&clock::clock(s), pill(13.0 * s));

    let power_icon = theme::icon_glyph("power_settings_new", 14.0 * s, theme::palette().accent);
    theme::on_palette_change({
        let icon = power_icon.downgrade();
        let s = s;
        move || {
            if let Some(icon) = icon.upgrade() {
                let fresh = theme::icon_glyph("power_settings_new", 14.0 * s, theme::palette().accent);
                icon.set_attributes(fresh.attributes().as_ref());
            }
        }
    });
    let power = Module::new(&power_icon, pill(10.0 * s));

    let right = gtk::Box::builder().spacing((8.0 * s).round() as i32).valign(gtk::Align::Center).margin_end(edge_margin).build();
    right.append(&power.widget);

    let content = gtk::CenterBox::builder().valign(gtk::Align::Start).build();
    content.set_start_widget(Some(&left));
    content.set_center_widget(Some(&clock.widget));
    content.set_end_widget(Some(&right));
    content
}
