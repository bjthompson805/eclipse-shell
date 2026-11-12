//! Mixer popout, dropping from the volume glyph in the status cluster: the
//! default output and input, each with a device picker and a fader, and a
//! fader for every app that is playing.
//!
//! The graph comes from `pw-dump` and the volumes from `wpctl get-volume` (on
//! OSS a node's own Props don't carry the hardware volume), read once a second
//! while the card is open. Changes go through `wpctl`. The card is only rebuilt
//! when the graph changes shape; volumes and mutes are updated in place, so a
//! drag isn't interrupted.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::icons;
use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-mixer";
const TICK: Duration = Duration::from_secs(1);
const WIDTH: f64 = 340.0;
/// How long a fader ignores readings after it's moved, so a reading already
/// under way can't snap it back.
const SETTLE: Duration = Duration::from_millis(1500);
/// The most often a drag sends a volume.
const SEND_EVERY: Duration = Duration::from_millis(40);
const SCROLL_STEP: f64 = 0.05;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Output,
    Input,
}

#[derive(Clone)]
struct Node {
    id: u32,
    label: String,
    glyph: &'static str,
    /// For an app: its icon name, if it gave one, and the class to look one up by.
    icon: Option<String>,
    app: String,
    volume: f64,
    muted: bool,
}

#[derive(Default)]
struct Graph {
    outputs: Vec<Node>,
    inputs: Vec<Node>,
    streams: Vec<Node>,
    sink: Option<u32>,
    source: Option<u32>,
}

impl Graph {
    fn devices(&self, kind: Kind) -> &[Node] {
        match kind {
            Kind::Output => &self.outputs,
            Kind::Input => &self.inputs,
        }
    }

    fn current(&self, kind: Kind) -> Option<&Node> {
        let id = match kind {
            Kind::Output => self.sink,
            Kind::Input => self.source,
        }?;
        self.devices(kind).iter().find(|n| n.id == id)
    }

    fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.outputs.iter().chain(&self.inputs).chain(&self.streams)
    }

    /// Everything the card's layout depends on, i.e. all but volumes and mutes.
    fn shape(&self, picking: Option<Kind>) -> String {
        let mut shape = format!("{:?} {:?} {}|", self.sink, self.source, picking.map_or(0, |k| k as u8 + 1));
        for node in self.nodes() {
            shape += &format!("{} {} {:?} {}|", node.id, node.label, node.icon, node.app);
        }
        shape
    }
}

pub struct MixerPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
    /// The device picker that's open, if any.
    picking: Cell<Option<Kind>>,
    graph: RefCell<Rc<Graph>>,
    shape: RefCell<String>,
    faders: RefCell<HashMap<u32, Rc<Fader>>>,
}

impl MixerPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            generation: Cell::new(0),
            picking: Cell::new(None),
            graph: RefCell::default(),
            shape: RefCell::default(),
            faders: RefCell::default(),
        })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        self.picking.set(None);
        self.shape.borrow_mut().clear();
        let mixer = self.clone();
        glib::spawn_future_local(async move {
            mixer.refresh().await;
            mixer.popout.show(Anchor::Under(x));
        });

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let mixer = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match mixer.upgrade() {
            Some(m) if m.popout.is_open() && m.generation.get() == generation => {
                glib::spawn_future_local(async move { m.refresh().await });
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    async fn refresh(self: &Rc<Self>) {
        let graph = Rc::new(read().await);
        *self.graph.borrow_mut() = graph.clone();
        if *self.shape.borrow() == graph.shape(self.picking.get()) {
            let faders = self.faders.borrow();
            for node in graph.nodes() {
                if let Some(fader) = faders.get(&node.id) {
                    fader.show(node.volume, node.muted);
                }
            }
            return;
        }
        self.rebuild();
    }

    fn rebuild(self: &Rc<Self>) {
        let graph = self.graph.borrow().clone();
        *self.shape.borrow_mut() = graph.shape(self.picking.get());
        self.faders.borrow_mut().clear();
        let s = self.popout.scale();
        self.popout.set_content(&popout::card(&self.body(s, &graph)));
    }

    /// Runs `wpctl` with `args`, then reads everything again.
    fn change(self: &Rc<Self>, args: Vec<String>) {
        let mixer = self.clone();
        glib::spawn_future_local(async move {
            let mut argv = vec!["wpctl".to_owned()];
            argv.extend(args);
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let _ = output(&argv).await;
            if mixer.popout.is_open() {
                mixer.refresh().await;
            }
        });
    }

    fn body(self: &Rc<Self>, s: f64, g: &Graph) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(11.0))
            .width_request(px(WIDTH) - 2 * px(16.0))
            .margin_top(px(13.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();

        let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        header.append(&theme::filled_icon_glyph("volume_up", 16.0 * s, theme::palette().accent));
        header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("MIXER"));
        column.append(&header);

        column.append(&self.device_section(s, g, Kind::Output));
        column.append(&popout::divider());
        column.append(&self.device_section(s, g, Kind::Input));
        column.append(&popout::divider());

        let apps = gtk::Box::new(gtk::Orientation::Vertical, px(7.0));
        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(7.0));
        head.append(&popout::micro_label(s, "Apps"));
        if !g.streams.is_empty() {
            head.append(&TextStyle::new(9.0 * s, pango::Weight::Semibold, FAINT).mono().label(&g.streams.len().to_string()));
        }
        apps.append(&head);
        if g.streams.is_empty() {
            let none = TextStyle::new(11.0 * s, pango::Weight::Medium, FAINT).label("Nothing playing");
            none.set_xalign(0.0);
            apps.append(&none);
        } else {
            let rows = gtk::Box::new(gtk::Orientation::Vertical, px(9.0));
            for node in &g.streams {
                rows.append(&self.app_row(s, node));
            }
            apps.append(&rows);
        }
        column.append(&apps);
        column
    }

    /// The default device of `kind`: its name with a picker chevron, its
    /// fader, and the picker itself while open.
    fn device_section(self: &Rc<Self>, s: f64, g: &Graph, kind: Kind) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let section = gtk::Box::new(gtk::Orientation::Vertical, px(7.0));
        section.append(&popout::micro_label(s, if kind == Kind::Output { "Output" } else { "Input" }));

        let node = g.current(kind);
        let candidates = g.devices(kind);
        let expanded = self.picking.get() == Some(kind);
        let (text, icon) = if expanded { (CREAM, CREAM) } else { (SUBTLE, ICON_DIM) };

        let head = gtk::Box::builder().spacing(px(8.0)).height_request(px(20.0)).build();
        let fallback = if kind == Kind::Output { "speaker" } else { "mic" };
        head.append(&theme::icon_glyph(node.map_or(fallback, |n| n.glyph), 15.0 * s, icon));
        let none = if kind == Kind::Output { "No output" } else { "No input" };
        let name = TextStyle::new(11.5 * s, pango::Weight::Semibold, text).label(node.map_or(none, |n| &n.label));
        fill(&name);
        head.append(&name);
        if candidates.len() > 1 {
            head.append(&TextStyle::new(9.0 * s, pango::Weight::Semibold, FAINT).label(&candidates.len().to_string()));
        }
        if !candidates.is_empty() {
            head.append(&theme::icon_glyph(if expanded { "expand_less" } else { "expand_more" }, 14.0 * s, icon));
            let mixer = Rc::downgrade(self);
            on_click(&head, move || {
                if let Some(m) = mixer.upgrade() {
                    m.picking.set(if expanded { None } else { Some(kind) });
                    m.rebuild();
                }
            });
        }

        let device = gtk::Box::new(gtk::Orientation::Vertical, px(6.0));
        device.append(&head);
        if let Some(node) = node {
            let glyphs = if kind == Kind::Output { ("volume_up", "volume_off") } else { ("mic", "mic_off") };
            let fader = Fader::new(s, node, glyphs, 17.0 * s, true);
            device.append(&fader.widget);
            self.faders.borrow_mut().insert(node.id, fader);
        }
        if expanded {
            let list = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
            for candidate in candidates {
                let current = node.is_some_and(|n| n.id == candidate.id);
                let mixer = Rc::downgrade(self);
                let id = candidate.id;
                list.append(&pick_row(s, candidate, current, move || {
                    if let Some(m) = mixer.upgrade() {
                        m.picking.set(None);
                        m.change(vec!["set-default".into(), id.to_string()]);
                    }
                }));
            }
            device.append(&list);
        }
        section.append(&device);
        section
    }

    /// An app that's playing: its icon, name and a mute glyph over a fader.
    fn app_row(self: &Rc<Self>, s: f64, node: &Node) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let row = gtk::Box::new(gtk::Orientation::Vertical, px(3.0));
        let head = gtk::Box::builder().spacing(px(9.0)).height_request(px(18.0)).build();
        let icon = match &node.icon {
            Some(name) => gio::ThemedIcon::from_names(&[name, "application-x-executable"]).upcast(),
            None => icons::for_class(&node.app),
        };
        let image = gtk::Image::from_gicon(&icon);
        image.set_pixel_size(px(16.0));
        head.append(&image);
        let name = TextStyle::new(10.5 * s, pango::Weight::Semibold, SUBTLE).label(&node.label);
        fill(&name);
        head.append(&name);
        let fader = Fader::new(s, node, ("volume_up", "volume_off"), 15.0 * s, false);
        head.append(&fader.glyph);
        row.append(&head);
        row.append(&fader.widget);
        on_hover(&row, move |lit| TextStyle::new(10.5 * s, pango::Weight::Semibold, if lit { CREAM } else { SUBTLE }).apply(&name));
        self.faders.borrow_mut().insert(node.id, fader);
        row
    }
}

// ---- readings ----------------------------------------------------------------

async fn read() -> Graph {
    let mut graph = Graph::default();
    let Some(dump) = output(&["pw-dump"]).await else { return graph };
    let Ok(objects) = serde_json::from_str::<Vec<serde_json::Value>>(&dump) else { return graph };

    let mut defaults: HashMap<String, String> = HashMap::new();
    let mut ids: HashMap<String, u32> = HashMap::new();
    for object in &objects {
        let kind = object["type"].as_str().unwrap_or("");
        if kind.ends_with(":Metadata") {
            for entry in object["metadata"].as_array().into_iter().flatten() {
                if let (Some(key), Some(name)) = (entry["key"].as_str(), entry["value"]["name"].as_str()) {
                    defaults.insert(key.to_owned(), name.to_owned());
                }
            }
            continue;
        }
        if !kind.ends_with(":Node") {
            continue;
        }
        let Some(id) = object["id"].as_u64().and_then(|id| u32::try_from(id).ok()) else { continue };
        let props = &object["info"]["props"];
        let prop = |key: &str| props[key].as_str().filter(|v| !v.is_empty());
        let name = prop("node.name").unwrap_or("");
        ids.insert(name.to_owned(), id);
        let device_label = || prop("node.description").or(prop("node.nick")).unwrap_or(name).to_owned();
        let mut node = Node { id, label: String::new(), glyph: "speaker", icon: None, app: String::new(), volume: 0.0, muted: false };
        match prop("media.class").unwrap_or("") {
            "Audio/Sink" => {
                node.label = device_label();
                node.glyph = device_glyph(Kind::Output, props);
                graph.outputs.push(node);
            }
            "Audio/Source" => {
                node.label = device_label();
                node.glyph = device_glyph(Kind::Input, props);
                graph.inputs.push(node);
            }
            "Stream/Output/Audio" => {
                node.label = prop("application.name").or(prop("media.name")).or(prop("node.description")).unwrap_or("Application").to_owned();
                node.icon = prop("application.icon-name").map(String::from);
                node.app = prop("application.process.binary").or(prop("application.name")).unwrap_or("").to_owned();
                graph.streams.push(node);
            }
            _ => {}
        }
    }
    graph.sink = defaults.get("default.audio.sink").and_then(|n| ids.get(n)).copied();
    graph.source = defaults.get("default.audio.source").and_then(|n| ids.get(n)).copied();

    // "48 Volume: 0.56 [MUTED]"
    let mut argv = vec!["sh".to_owned(), "-c".into(), r#"for i; do printf '%s ' "$i"; wpctl get-volume "$i"; done"#.into(), "sh".into()];
    argv.extend(graph.nodes().map(|n| n.id.to_string()));
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let volumes = output(&argv).await.unwrap_or_default();
    let volumes: HashMap<u32, (f64, bool)> = volumes
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let id = words.next()?.parse().ok()?;
            let volume = words.nth(1)?.parse().ok()?;
            Some((id, (volume, line.contains("MUTED"))))
        })
        .collect();
    for node in graph.outputs.iter_mut().chain(&mut graph.inputs).chain(&mut graph.streams) {
        if let Some(&(volume, muted)) = volumes.get(&node.id) {
            (node.volume, node.muted) = (volume, muted);
        }
    }
    graph
}

fn device_glyph(kind: Kind, props: &serde_json::Value) -> &'static str {
    let hint = ["device.api", "device.icon-name", "node.name", "node.description"].map(|k| props[k].as_str().unwrap_or("")).join(" ").to_lowercase();
    match () {
        _ if ["bluez", "headphone", "headset"].iter().any(|h| hint.contains(h)) => "headphones",
        _ if ["hdmi", "displayport"].iter().any(|h| hint.contains(h)) => "tv",
        _ if kind == Kind::Input => "mic",
        _ => "speaker",
    }
}

async fn output(argv: &[&str]) -> Option<String> {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    stdout.map(String::from)
}

fn spawn(argv: &[&str]) {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    if let Ok(p) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        p.wait_async(gio::Cancellable::NONE, |_| {});
    }
}

// ---- fader -------------------------------------------------------------------

/// A thin track with an accent fill and a resting tick, a percentage, and a
/// glyph that mutes. Drag or scroll the track to change the volume.
struct Fader {
    id: u32,
    s: f64,
    value: Cell<f64>,
    muted: Cell<bool>,
    lit: Cell<bool>,
    dragging: Cell<bool>,
    /// When the user last changed it.
    touched: Cell<Option<Instant>>,
    sent: Cell<Option<Instant>>,
    widget: gtk::Box,
    track: gtk::DrawingArea,
    readout: gtk::Label,
    /// The mute glyph, inside `widget` or for the caller to place.
    glyph: gtk::Label,
    glyphs: (&'static str, &'static str),
    glyph_px: f64,
}

impl Fader {
    fn new(s: f64, node: &Node, glyphs: (&'static str, &'static str), glyph_px: f64, inline: bool) -> Rc<Self> {
        let px = |v: f64| (v * s).round() as i32;
        let widget = gtk::Box::builder().spacing(px(11.0)).height_request(px(28.0)).build();
        let glyph = theme::icon_glyph(glyphs.0, glyph_px, ICON_DIM);
        glyph.set_size_request(px(20.0), px(20.0));
        glyph.set_cursor_from_name(Some("pointer"));
        let track = gtk::DrawingArea::builder().hexpand(true).build();
        track.set_cursor_from_name(Some("pointer"));
        let readout = gtk::Label::new(None);
        readout.set_size_request(px(34.0), -1);
        readout.set_xalign(1.0);
        if inline {
            widget.append(&glyph);
        }
        widget.append(&track);
        widget.append(&readout);

        let fader = Rc::new(Self {
            id: node.id,
            s,
            value: Cell::new(node.volume.clamp(0.0, 1.0)),
            muted: Cell::new(node.muted),
            lit: Cell::new(false),
            dragging: Cell::new(false),
            touched: Cell::new(None),
            sent: Cell::new(None),
            widget,
            track,
            readout,
            glyph,
            glyphs,
            glyph_px,
        });

        let weak = Rc::downgrade(&fader);
        fader.track.set_draw_func(move |_, cr, w, h| {
            if let Some(f) = weak.upgrade() {
                f.draw(cr, f64::from(w), f64::from(h));
            }
        });

        let weak = Rc::downgrade(&fader);
        on_click(&fader.glyph, move || {
            if let Some(f) = weak.upgrade() {
                f.muted.set(!f.muted.get());
                f.touched.set(Some(Instant::now()));
                spawn(&["wpctl", "set-mute", &f.id.to_string(), if f.muted.get() { "1" } else { "0" }]);
                f.repaint();
            }
        });

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

        // Only the fader itself lights it, so hovering or clicking the
        // device picker above leaves the tick alone.
        let weak = Rc::downgrade(&fader);
        on_hover(&fader.widget, move |lit| weak.upgrade().into_iter().for_each(|f| f.set_lit(lit)));

        fader.repaint();
        fader
    }

    /// Takes a reading, unless the user is moving it.
    fn show(&self, volume: f64, muted: bool) {
        let settling = self.touched.get().is_some_and(|t| t.elapsed() < SETTLE);
        if self.dragging.get() || settling {
            return;
        }
        let volume = volume.clamp(0.0, 1.0);
        if (volume - self.value.get()).abs() > 0.001 || muted != self.muted.get() {
            self.value.set(volume);
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

    /// Moves the fader; `force` sends the volume even if one went out just now.
    fn set_value(&self, value: f64, force: bool) {
        let value = value.clamp(0.0, 1.0);
        self.value.set(value);
        self.touched.set(Some(Instant::now()));
        if force || self.sent.get().is_none_or(|t| t.elapsed() >= SEND_EVERY) {
            self.sent.set(Some(Instant::now()));
            spawn(&["wpctl", "set-volume", &self.id.to_string(), &format!("{value:.3}")]);
        }
        self.repaint();
    }

    fn repaint(&self) {
        let (muted, lit) = (self.muted.get(), self.lit.get());
        let text = if muted { "off".to_owned() } else { format!("{}%", (self.value.get() * 100.0).round()) };
        self.readout.set_text(&text);
        let color = if muted { FAINT } else if lit { CREAM } else { DIM };
        TextStyle::new(9.5 * self.s, pango::Weight::Semibold, color).apply(&self.readout);
        let name = if muted { self.glyphs.1 } else { self.glyphs.0 };
        self.glyph.set_text(name);
        let color = if muted { FAINT } else if lit { CREAM } else { ICON_DIM };
        self.glyph.set_attributes(theme::icon_glyph(name, self.glyph_px, color).attributes().as_ref());
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

// ---- pieces ------------------------------------------------------------------

/// A device in the picker: its glyph and name, with a dot on the current one.
fn pick_row(s: f64, node: &Node, current: bool, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let hovered = Rc::new(Cell::new(false));
    let background = gtk::DrawingArea::new();
    background.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            if hovered.get() {
                theme::rounded_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), 8.0 * s);
                theme::set_source(cr, theme::palette().raised);
                let _ = cr.fill();
            }
        }
    });

    let accent = theme::palette().accent_lit;
    let content = gtk::Box::builder().spacing(px(9.0)).height_request(px(26.0)).margin_start(px(9.0)).margin_end(px(11.0)).build();
    content.append(&theme::icon_glyph(node.glyph, 14.0 * s, if current { accent } else { ICON_DIM }));
    let (weight, color) = if current { (pango::Weight::Semibold, CREAM) } else { (pango::Weight::Medium, SUBTLE) };
    let name = TextStyle::new(10.5 * s, weight, color).label(&node.label);
    fill(&name);
    content.append(&name);
    if current {
        let dot = gtk::DrawingArea::builder().content_width(px(5.0)).content_height(px(5.0)).valign(gtk::Align::Center).build();
        dot.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
            theme::set_source(cr, accent);
            let _ = cr.fill();
        });
        content.append(&dot);
    }

    let row = gtk::Overlay::new();
    row.set_child(Some(&background));
    row.add_overlay(&content);
    row.set_measure_overlay(&content, true);
    on_hover(&row, move |inside| {
        hovered.set(inside);
        background.queue_draw();
    });
    on_click(&row, pressed);
    row
}

/// Lets a label fill what its row leaves it, ellipsized, rather than
/// widening the card to fit its whole text.
fn fill(label: &gtk::Label) {
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
}

fn on_hover(widget: &impl IsA<gtk::Widget>, f: impl Fn(bool) + 'static) {
    let f = Rc::new(f);
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter({
        let f = f.clone();
        move |_, _, _| f(true)
    });
    motion.connect_leave(move |_| f(false));
    widget.add_controller(motion);
}

fn on_click(widget: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    widget.set_cursor_from_name(Some("pointer"));
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| f());
    widget.add_controller(click);
}
