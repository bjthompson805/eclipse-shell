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
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::audio::{self, Graph, Kind, Node};
use crate::fader::Fader;
use crate::icons;
use crate::popout::{self, on_click, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, FAINT, ICON_DIM, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-mixer";
const TICK: Duration = Duration::from_secs(1);
const WIDTH: f64 = 340.0;


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
        let graph = Rc::new(audio::read().await);
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
            let _ = audio::output(&argv).await;
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
        popout::fill(&name);
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
            let fader = audio::volume_fader(s, node, glyphs, 17.0 * s, true);
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
        popout::fill(&name);
        head.append(&name);
        let fader = audio::volume_fader(s, node, ("volume_up", "volume_off"), 15.0 * s, false);
        head.append(&fader.glyph);
        row.append(&head);
        row.append(&fader.widget);
        on_hover(&row, move |lit| TextStyle::new(10.5 * s, pango::Weight::Semibold, if lit { CREAM } else { SUBTLE }).apply(&name));
        self.faders.borrow_mut().insert(node.id, fader);
        row
    }
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
    popout::fill(&name);
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

