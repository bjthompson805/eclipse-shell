//! Per-monitor bar: a frame around the whole screen whose top edge thickens
//! into a band carrying the modules.
//!
//! Two layer surfaces per monitor: a full-screen one that draws the frame and
//! the bar (taking input only on the band), and an invisible strip that
//! reserves the band's height so tiled windows sit below it.

mod agent_usage;
mod bell;
mod clock;
mod media;
mod module;
mod net_speed;
mod status;
mod stats;
mod title;
mod tray;
mod weather;
mod workspaces;

pub use clock::every_second;

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config;
use crate::hyprland::State;
use crate::hint;
use crate::mark;
use crate::theme;
use module::{Module, ModuleSpec};
use title::Title;
use workspaces::Workspaces;

/// What the bar's buttons open. The f64 is the button's centre, for
/// popouts that drop beneath it.
pub struct Actions {
    pub launcher: Rc<dyn Fn()>,
    pub power: Rc<dyn Fn()>,
    pub calendar: Rc<dyn Fn(f64)>,
    pub usage: Rc<dyn Fn(f64)>,
    pub mixer: Rc<dyn Fn(f64)>,
    pub display: Rc<dyn Fn(f64)>,
    pub network: Rc<dyn Fn(f64)>,
    pub bluetooth: Rc<dyn Fn(f64)>,
    pub media: Rc<dyn Fn(f64)>,
    pub weather: Rc<dyn Fn(f64)>,
    pub traffic: Rc<dyn Fn(f64)>,
    pub workspaces: Rc<dyn Fn(f64)>,
    pub inbox: Rc<dyn Fn(f64)>,
    pub battery: Rc<dyn Fn(f64)>,
    pub window: Rc<dyn Fn(f64)>,
    pub resources: Rc<dyn Fn(f64)>,
    /// The volume (true) or brightness (false) just changed: flash the OSD.
    pub level: Rc<dyn Fn(bool)>,
    /// The pointer moved over a monitor's bar, to (x, y), or left it; the
    /// sidebars open from its outer corners.
    pub bar_pointer: Rc<dyn Fn(&gdk::Monitor, Option<(f64, f64)>)>,
}

/// Bar modules that popouts drop beneath, for opening them by command.
#[derive(Default)]
struct Anchors {
    stats: Option<gtk::Widget>,
    usage: Option<gtk::Widget>,
    volume: Option<gtk::Widget>,
    display: Option<gtk::Widget>,
    network: Option<gtk::Widget>,
    bluetooth: Option<gtk::Widget>,
    media: Option<gtk::Widget>,
    weather: Option<gtk::Widget>,
    traffic: Option<gtk::Widget>,
    bell: Option<gtk::Widget>,
    battery: Option<gtk::Widget>,
}

pub struct Bar {
    pub connector: Option<String>,
    windows: [gtk::ApplicationWindow; 2],
    workspaces: Rc<Workspaces>,
    title: Rc<Title>,
    clock: gtk::Widget,
    anchors: Anchors,
}

impl Bar {
    pub fn new(app: &gtk::Application, monitor: &gdk::Monitor, actions: &Rc<Actions>) -> Self {
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
        // Hover hints float just under the band.
        let hints = hint::layer();
        let (content, anchors) = band_content(s, band, &workspaces, &title, actions, &hints, top + 6.0 * s);
        content.set_size_request(-1, top.round() as i32);

        let layers = gtk::Overlay::new();
        layers.set_child(Some(&canvas));
        layers.add_overlay(&content);
        layers.add_overlay(&hints);
        frame.set_child(Some(&layers));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let (actions, monitor) = (actions.clone(), monitor.clone());
            move |_, x, y| (actions.bar_pointer)(&monitor, Some((x, y)))
        });
        motion.connect_leave({
            let (actions, monitor) = (actions.clone(), monitor.clone());
            move |_| (actions.bar_pointer)(&monitor, None)
        });
        frame.add_controller(motion);

        reserve.present();
        frame.present();
        // The clock floats as the band's last overlay.
        let clock = content.last_child().expect("clock");
        Self { connector: monitor.connector().map(String::from), windows: [reserve, frame], workspaces, title, clock, anchors }
    }

    pub fn update(&self, state: &State) {
        self.workspaces.update(state, self.connector.as_deref());
        self.title.update(state.focused.as_ref());
    }

    /// Where the clock's centre sits, in logical px from the monitor's left.
    pub fn clock_x(&self) -> Option<f64> {
        centre_x(&self.clock)
    }

    /// Where the window title's centre sits.
    pub fn title_x(&self) -> Option<f64> {
        centre_x(self.title.widget.upcast_ref())
    }

    /// Where the stats readout's centre sits, if the bar shows it.
    pub fn stats_x(&self) -> Option<f64> {
        centre_x(self.anchors.stats.as_ref()?)
    }

    /// Where the volume glyph's centre sits, if the bar shows it.
    pub fn volume_x(&self) -> Option<f64> {
        centre_x(self.anchors.volume.as_ref()?)
    }

    /// Where the sun's centre sits, if the bar shows it.
    pub fn display_x(&self) -> Option<f64> {
        self.anchors.display.as_ref().filter(|d| d.is_visible()).and_then(centre_x)
    }

    /// Where the network glyph's centre sits, if the bar shows it.
    pub fn network_x(&self) -> Option<f64> {
        centre_x(self.anchors.network.as_ref()?)
    }

    /// Where the Bluetooth glyph's centre sits, if the bar shows it.
    pub fn bluetooth_x(&self) -> Option<f64> {
        self.anchors.bluetooth.as_ref().filter(|b| b.is_visible()).and_then(centre_x)
    }

    /// Where the now-playing pill's centre sits, if the bar shows it.
    pub fn media_x(&self) -> Option<f64> {
        self.anchors.media.as_ref().filter(|m| m.is_visible()).and_then(centre_x)
    }

    /// Where the weather pill's centre sits, if the bar shows it.
    pub fn weather_x(&self) -> Option<f64> {
        self.anchors.weather.as_ref().filter(|w| w.is_visible()).and_then(centre_x)
    }

    /// Where the net-speed readout's centre sits, if the bar shows it.
    pub fn traffic_x(&self) -> Option<f64> {
        centre_x(self.anchors.traffic.as_ref()?)
    }

    /// Where the workspace pills' centre sits.
    pub fn workspaces_x(&self) -> Option<f64> {
        centre_x(self.workspaces.widget.upcast_ref())
    }

    /// Where the notification bell's centre sits.
    pub fn bell_x(&self) -> Option<f64> {
        centre_x(self.anchors.bell.as_ref()?)
    }

    /// Where the battery's centre sits, if the bar shows it.
    pub fn battery_x(&self) -> Option<f64> {
        self.anchors.battery.as_ref().filter(|b| b.is_visible()).and_then(centre_x)
    }

    /// Where the usage readout's centre sits, if the bar shows it.
    pub fn usage_x(&self) -> Option<f64> {
        centre_x(self.anchors.usage.as_ref()?)
    }

    pub fn destroy(&self) {
        for window in &self.windows {
            window.destroy();
        }
    }
}

/// A widget's horizontal centre in its window. The bar's surface spans the
/// monitor, so that is also its distance from the monitor's left edge.
pub(super) fn centre_x(widget: &gtk::Widget) -> Option<f64> {
    let root = widget.root()?;
    let point = widget.compute_point(&root, &gtk::graphene::Point::new(widget.width() as f32 / 2.0, 0.0))?;
    Some(f64::from(point.x()))
}

/// Waits for whichever finishes first.
pub(crate) async fn first_of(a: impl std::future::Future<Output = ()>, b: impl std::future::Future) {
    let (a, b) = (std::pin::pin!(a), std::pin::pin!(b));
    let mut both = (a, b);
    std::future::poll_fn(|cx| {
        if both.0.as_mut().poll(cx).is_ready() || both.1.as_mut().poll(cx).is_ready() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
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

/// The band: mark, workspaces, system stats, agent usage and title on the left, the clock centred, and
/// now playing, network speed, the status cluster, weather, the tray and the power button on the right.
/// `hints` is the layer hover hints float on, with their tops at `hint_y`.
fn band_content(s: f64, band: f64, workspaces: &Rc<Workspaces>, title: &Rc<Title>, actions: &Rc<Actions>, hints: &gtk::Fixed, hint_y: f64) -> (gtk::Overlay, Anchors) {
    let mut anchors = Anchors::default();
    let span = (band * 0.76).round();
    let edge_margin = (24.0 * s).round() as i32;
    let pill = |pad_x: f64| ModuleSpec { span, pad_x, filled: true, interactive: true, square: false };

    let mark = Module::new(&mark::mark(15.0 * s), ModuleSpec { filled: false, square: true, ..pill(0.0) });
    mark.connect_tapped({
        let actions = actions.clone();
        move || (actions.launcher)()
    });
    let spaces = Module::new(&workspaces.widget, ModuleSpec { interactive: false, ..pill(10.0 * s) });
    // A left click on a pill switches to it; a right click opens the switcher.
    let right_click = gtk::GestureClick::builder().button(gtk::gdk::BUTTON_SECONDARY).build();
    right_click.connect_released({
        let (actions, pills) = (actions.clone(), workspaces.widget.downgrade());
        move |_, _, _, _| {
            if let Some(x) = pills.upgrade().and_then(|p| centre_x(p.upcast_ref())) {
                (actions.workspaces)(x);
            }
        }
    });
    spaces.widget.add_controller(right_click);

    let left = gtk::Box::builder().spacing((8.0 * s).round() as i32).valign(gtk::Align::Center).margin_start(edge_margin).build();
    left.append(&mark.widget);
    left.append(&spaces.widget);
    if config::BAR_STATS {
        let stats = Module::new(&stats::stats(s), pill(6.0 * s));
        stats.connect_tapped({
            let (widget, actions) = (stats.widget.downgrade(), actions.clone());
            move || {
                if let Some(x) = widget.upgrade().and_then(|w| centre_x(w.upcast_ref())) {
                    (actions.resources)(x);
                }
            }
        });
        left.append(&stats.widget);
        anchors.stats = Some(stats.widget.upcast());
    }
    anchors.usage = config::BAR_AGENT_USAGE.then(|| {
        let usage = Module::new(&agent_usage::agent_usage(s), pill(6.0 * s));
        usage.connect_tapped({
            let (widget, actions) = (usage.widget.downgrade(), actions.clone());
            move || {
                if let Some(x) = widget.upgrade().and_then(|w| centre_x(w.upcast_ref())) {
                    (actions.usage)(x);
                }
            }
        });
        left.append(&usage.widget);
        usage.widget.upcast::<gtk::Widget>()
    });
    left.append(&title.widget);
    title.widget.set_cursor_from_name(Some("pointer"));
    let click = gtk::GestureClick::new();
    click.connect_released({
        let actions = actions.clone();
        move |gesture, _, _, _| {
            if let Some(x) = gesture.widget().and_then(|w| centre_x(&w)) {
                (actions.window)(x);
            }
        }
    });
    title.widget.add_controller(click);

    let clock = Module::new(&clock::clock(s), pill(13.0 * s));
    clock.connect_tapped({
        let (widget, actions) = (clock.widget.downgrade(), actions.clone());
        move || {
            if let Some(x) = widget.upgrade().and_then(|w| centre_x(w.upcast_ref())) {
                (actions.calendar)(x);
            }
        }
    });

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
    power.connect_tapped({
        let actions = actions.clone();
        move || (actions.power)()
    });

    let right = gtk::Box::builder().spacing((8.0 * s).round() as i32).valign(gtk::Align::Center).margin_end(edge_margin).build();
    if config::BAR_MEDIA {
        let playing = media::media(s);
        let media = Module::new(&playing, pill(12.0 * s));
        media.connect_tapped({
            let (actions, pill) = (actions.clone(), media.widget.downgrade());
            move || {
                if let Some(x) = pill.upgrade().and_then(|p| centre_x(p.upcast_ref())) {
                    (actions.media)(x);
                }
            }
        });
        // A middle click plays or pauses without opening anything.
        let middle = gtk::GestureClick::builder().button(gtk::gdk::BUTTON_MIDDLE).build();
        middle.connect_released(|_, _, _, _| media::toggle());
        media.widget.add_controller(middle);
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let level = actions.level.clone();
        scroll.connect_scroll(move |_, _, dy| {
            if dy != 0.0 {
                let level = level.clone();
                status::nudge_volume(dy < 0.0, move || level(true));
            }
            gtk::glib::Propagation::Stop
        });
        media.widget.add_controller(scroll);
        playing.bind_property("visible", &media.widget, "visible").sync_create().build();
        anchors.media = Some(media.widget.clone().upcast());
        right.append(&media.widget);
    }
    if config::BAR_NET_SPEED {
        let speed = Module::new(&net_speed::net_speed(s), pill(6.0 * s));
        speed.connect_tapped({
            let (actions, pill) = (actions.clone(), speed.widget.downgrade());
            move || {
                if let Some(x) = pill.upgrade().and_then(|p| centre_x(p.upcast_ref())) {
                    (actions.traffic)(x);
                }
            }
        });
        anchors.traffic = Some(speed.widget.clone().upcast());
        right.append(&speed.widget);
    }
    if config::BAR_STATUS {
        let cluster = status::status(s, actions);
        // Volume, the sun, network and Bluetooth lead the cluster; the battery ends it.
        anchors.volume = cluster.first_child();
        anchors.display = anchors.volume.as_ref().and_then(|v| v.next_sibling());
        anchors.network = anchors.display.as_ref().and_then(|d| d.next_sibling());
        anchors.bluetooth = anchors.network.as_ref().and_then(|n| n.next_sibling());
        anchors.battery = cluster.last_child();
        let status = Module::new(&cluster, ModuleSpec { interactive: false, ..pill(8.0 * s) });
        right.append(&status.widget);
    }
    if config::BAR_WEATHER {
        let reading = weather::weather(s);
        let weather = Module::new(&reading, pill(12.0 * s));
        weather.connect_tapped({
            let (actions, pill) = (actions.clone(), weather.widget.downgrade());
            move || {
                if let Some(x) = pill.upgrade().and_then(|p| centre_x(p.upcast_ref())) {
                    (actions.weather)(x);
                }
            }
        });
        anchors.weather = Some(weather.widget.clone().upcast());
        // The pill only shows once there is a reading to put in it.
        reading.bind_property("visible", &weather.widget, "visible").sync_create().build();
        right.append(&weather.widget);
    }
    if config::BAR_TRAY {
        let icons = tray::tray(s, hints, hint_y);
        let tray = Module::new(&icons, ModuleSpec { interactive: false, ..pill(11.0 * s) });
        icons.bind_property("visible", &tray.widget, "visible").sync_create().build();
        right.append(&tray.widget);
    }
    let bell = Module::new(&bell::bell(s), pill(10.0 * s));
    bell.connect_tapped({
        let (actions, pill) = (actions.clone(), bell.widget.downgrade());
        move || {
            if let Some(x) = pill.upgrade().and_then(|p| centre_x(p.upcast_ref())) {
                (actions.inbox)(x);
            }
        }
    });
    anchors.bell = Some(bell.widget.clone().upcast());
    right.append(&bell.widget);
    right.append(&power.widget);

    // The sides sit at the edges; the clock floats above them at the centre,
    // sliding sideways only as far as keeps a gap from a crowded side.
    let sides = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    left.set_hexpand(true);
    left.set_halign(gtk::Align::Start);
    sides.append(&left);
    sides.append(&right);
    let content = gtk::Overlay::builder().valign(gtk::Align::Start).child(&sides).build();
    content.add_overlay(&clock.widget);
    let gap = (12.0 * s).round() as i32;
    content.connect_get_child_position(move |content, clock| {
        let natural = |w: &gtk::Widget| w.measure(gtk::Orientation::Horizontal, -1).1;
        let (width, height) = (content.width(), content.height());
        let cw = natural(clock);
        let lowest = natural(left.upcast_ref()) + gap;
        let highest = width - natural(right.upcast_ref()) - gap - cw;
        let x = (width / 2 - cw / 2).min(highest).max(lowest);
        Some(gdk::Rectangle::new(x, 0, cw, height))
    });
    (content, anchors)
}
