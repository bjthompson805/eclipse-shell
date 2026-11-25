//! The screenshot tool, `eclipse-shell screenshot [monitor]`: a port of
//! ryoku's ryoshot. Every monitor freezes under an overlay; drag a region,
//! click a window, or (monitor mode) click a monitor; annotate it from the
//! toolbar under the selection; then copy, save or upload it.
//!
//! It runs in the shell's own long-lived process, so the clipboard hand-off
//! outlives the overlay: ryoshot quit as soon as wl-copy returned, and the
//! copy was often gone by the time anything pasted. wl-copy now runs in a
//! process group of its own and stays to serve the paste.
//!
//! Speed, as ryoshot learned: the frozen screens are GPU textures and only
//! the selection's area redraws while drawing; exports encode their PNG on a
//! worker thread at a low zlib level (ryoshot's encode-out-of-process fix), so
//! the overlay never freezes. Copy hides the overlay as soon as the picture is
//! rendered and finishes the hand-off behind it; Save opens its dialog at once
//! and encodes while it's open.

mod ann;
mod icons;
mod toolbar;

use std::cell::{Cell, RefCell};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::hyprland;
use crate::theme::{self, Rgba, TextStyle};
use ann::{Ann, Kind, Model};
use toolbar::{Action, Toolbar, View};

const DIM: Rgba = Rgba::rgba8(15, 12, 7, 0.62);
/// Room around the selection's drawing area for its label and handles.
const PAD: f64 = 28.0;

thread_local! {
    static OPEN: RefCell<Option<Rc<Shot>>> = const { RefCell::new(None) };
}

/// A rectangle in global logical px.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn from_points(a: (f64, f64), b: (f64, f64)) -> Self {
        Self { x: a.0.min(b.0), y: a.1.min(b.1), w: (b.0 - a.0).abs(), h: (b.1 - a.1).abs() }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn intersect(&self, o: &Rect) -> Option<Rect> {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = ((self.x + self.w).min(o.x + o.w), (self.y + self.h).min(o.y + o.h));
        (x1 > x0 && y1 > y0).then_some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }

    fn clamp(&self, x: f64, y: f64) -> (f64, f64) {
        (x.clamp(self.x, self.x + self.w), y.clamp(self.y, self.y + self.h))
    }
}

/// A monitor, frozen.
struct Mon {
    name: String,
    area: Rect,
    /// Device px per logical px.
    scale: f64,
    image: cairo::ImageSurface,
    texture: gdk::MemoryTexture,
}

struct Overlay {
    window: gtk::ApplicationWindow,
    mon: usize,
    fixed: gtk::Fixed,
    dims: [gtk::DrawingArea; 4],
    /// The selection with its chrome and annotations.
    area: gtk::DrawingArea,
    /// Where `area` sits, in the overlay's coordinates.
    area_at: Cell<(f64, f64)>,
    /// The window or monitor under the pointer, before anything's chosen.
    hover: gtk::DrawingArea,
    toolbar: Rc<Toolbar>,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Selecting,
    Editing,
}

struct State {
    phase: Phase,
    sel: Option<Rect>,
    press: Option<(f64, f64)>,
    dragging: bool,
    hover: Option<Rect>,
    tool: &'static str,
    color: Rgba,
    width: f64,
    rough: bool,
    model: Model,
    draft: Option<Ann>,
    text_editing: bool,
    selected: Option<usize>,
    move_start: Option<(f64, f64)>,
    move_offset: (f64, f64),
    settings_open: bool,
    listening: bool,
    hotkey: String,
    /// Exporting: input is ignored from here on.
    busy: bool,
}

pub struct Shot {
    monitor_mode: bool,
    mons: Vec<Mon>,
    windows: Vec<(Rect, i64)>,
    overlays: RefCell<Vec<Overlay>>,
    state: RefCell<State>,
}

/// Starts a screenshot, unless one is open.
pub fn start(app: &gtk::Application, monitor_mode: bool, test: bool) {
    // A second press while one is up, or still capturing, does nothing.
    if active() {
        return;
    }
    let app = app.clone();
    CAPTURING.with(|c| c.set(true));
    glib::spawn_future_local(async move {
        let shot = capture(monitor_mode).await;
        CAPTURING.with(|c| c.set(false));
        if let Some(shot) = shot {
            shot.open(&app);
            OPEN.with_borrow_mut(|o| *o = Some(shot.clone()));
            if test {
                shot.test_drive();
            }
        }
    });
}

impl Shot {
    /// `screenshot --test`, as ryoshot's RYOSHOT_TESTRECT: selects a box on the
    /// first monitor, draws one of every annotation, holds 3s for a look, then
    /// copies.
    fn test_drive(self: &Rc<Self>) {
        let m = self.mons[0].area;
        let (bx, by) = (m.x + (m.w * 0.2).round(), m.y + (m.h * 0.2).round());
        {
            let mut st = self.state.borrow_mut();
            st.sel = Some(Rect { x: bx, y: by, w: 760.0, h: 480.0 });
            st.phase = Phase::Editing;
            let c = |hex: &str| Rgba::hex(hex).unwrap_or(toolbar::SWATCHES[0].0);
            let p = |x: f64, y: f64| (bx + x, by + y);
            st.model.add(Ann::new(Kind::Ellipse, vec![p(40.0, 40.0), p(240.0, 180.0)], c("#4f8fe0"), 4.0));
            st.model.add(Ann::new(Kind::Line, vec![p(300.0, 60.0), p(700.0, 200.0)], c("#f2c14e"), 7.0));
            st.model.add(Ann::new(Kind::Arrow, vec![p(60.0, 440.0), p(360.0, 260.0)], c("#e23b3b"), 5.0));
            let mut rough = Ann::new(Kind::Rect, vec![p(420.0, 250.0), p(560.0, 330.0)], c("#ffffff"), 4.0);
            rough.rough = true;
            st.model.add(rough);
            let pen = (0..=40).map(|i| f64::from(i) / 40.0).map(|t| p(300.0 + t * 380.0, 320.0 + (t * 6.2832).sin() * 60.0)).collect();
            st.model.add(Ann::new(Kind::Pen, pen, c("#5bbf73"), 3.0));
            st.model.add(Ann::new(Kind::Marker, vec![p(100.0, 400.0), p(660.0, 420.0)], ann::MARKER, 4.0));
            st.model.add(Ann::new(Kind::Blur, vec![p(0.0, 60.0), p(380.0, 150.0)], c("#ffffff"), 4.0));
            st.model.add(Ann::new(Kind::Pixelate, vec![p(0.0, 160.0), p(380.0, 230.0)], c("#ffffff"), 4.0));
            st.model.add(Ann::new(Kind::Magnify, vec![p(580.0, 40.0), p(720.0, 180.0)], c("#ffffff"), 4.0));
            let mut text = Ann::new(Kind::Text, vec![p(60.0, 20.0)], c("#ffffff"), 4.0);
            text.text = "eclipse shot test".into();
            text.size = 28.0;
            st.model.add(text);
            let mut step = Ann::new(Kind::Counter, vec![p(700.0, 430.0)], c("#e2342a"), 4.0);
            step.n = 1;
            st.model.add(step);
        }
        self.layout();
        let this = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || this.action(Action::Copy));
    }
}

thread_local! {
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
}

/// Whether a screenshot is open or being taken.
pub fn active() -> bool {
    CAPTURING.with(Cell::get) || OPEN.with_borrow(Option::is_some)
}

// ---- capture ----------------------------------------------------------------------

struct Captured {
    name: String,
    x: f64,
    y: f64,
    scale: f64,
    width: i32,
    height: i32,
    rgb: Vec<u8>,
}

async fn capture(monitor_mode: bool) -> Option<Rc<Shot>> {
    let (monitors, clients) = gio::spawn_blocking(|| (hyprland::request("j/monitors").ok(), hyprland::request("j/clients").ok())).await.ok()?;
    let monitors: serde_json::Value = serde_json::from_str(&monitors?).ok()?;
    let monitors: Vec<(String, f64, f64, f64)> = monitors
        .as_array()?
        .iter()
        .filter(|m| !m["disabled"].as_bool().unwrap_or(false))
        .filter_map(|m| Some((m["name"].as_str()?.to_owned(), m["x"].as_f64()?, m["y"].as_f64()?, m["scale"].as_f64().unwrap_or(1.0))))
        .collect();
    let active_ws: Vec<i64> = serde_json::from_str::<serde_json::Value>(&hyprland::request("j/monitors").unwrap_or_default())
        .ok()
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|m| m["activeWorkspace"]["id"].as_i64()).collect()))
        .unwrap_or_default();
    let windows = clients.and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok()).map(|v| parse_windows(&v, &active_ws)).unwrap_or_default();

    if !gio::spawn_blocking(|| crate::deps::ensure("grim", "grim")).await.unwrap_or(false) {
        return None;
    }
    // Every monitor at once, as uncompressed PPM: grim's fastest output.
    let grabs: Vec<_> = monitors
        .into_iter()
        .map(|(name, x, y, scale)| {
            gio::spawn_blocking(move || {
                let out = Command::new("grim").args(["-t", "ppm", "-o", &name, "-"]).stderr(Stdio::null()).output().ok()?;
                let (width, height, rgb) = parse_ppm(out.stdout)?;
                Some(Captured { name, x, y, scale, width, height, rgb })
            })
        })
        .collect();
    let mut mons = Vec::new();
    for grab in grabs {
        let Ok(Some(c)) = grab.await else { continue };
        mons.push(mon(c)?);
    }
    if mons.is_empty() {
        notify("Screenshot failed", "grim couldn't capture the screen.");
        return None;
    }
    let state = State {
        phase: Phase::Selecting,
        sel: None,
        press: None,
        dragging: false,
        hover: None,
        tool: "rect",
        color: toolbar::SWATCHES[0].0,
        width: 4.0,
        rough: false,
        model: Model::default(),
        draft: None,
        text_editing: false,
        selected: None,
        move_start: None,
        move_offset: (0.0, 0.0),
        settings_open: false,
        listening: false,
        hotkey: hotkey::current().unwrap_or_else(|| "\u{2014}".to_owned()),
        busy: false,
    };
    Some(Rc::new(Shot { monitor_mode, mons, windows, overlays: RefCell::default(), state: RefCell::new(state) }))
}

fn parse_windows(clients: &serde_json::Value, active_ws: &[i64]) -> Vec<(Rect, i64)> {
    clients
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["mapped"].as_bool().unwrap_or(false) && !c["hidden"].as_bool().unwrap_or(false))
        .filter(|c| c["workspace"]["id"].as_i64().is_some_and(|id| active_ws.contains(&id)))
        .filter_map(|c| {
            let (at, size) = (c["at"].as_array()?, c["size"].as_array()?);
            let r = Rect { x: at[0].as_f64()?, y: at[1].as_f64()?, w: size[0].as_f64()?, h: size[1].as_f64()? };
            (r.w > 0.0 && r.h > 0.0).then(|| (r, c["focusHistoryID"].as_i64().unwrap_or(i64::MAX)))
        })
        .collect()
}

/// A binary PPM (P6, maxval 255): width, height and the RGB bytes.
fn parse_ppm(data: Vec<u8>) -> Option<(i32, i32, Vec<u8>)> {
    let mut fields = Vec::new();
    let mut i = 2;
    if !data.starts_with(b"P6") {
        return None;
    }
    while fields.len() < 3 {
        while data.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        if data[i] == b'#' {
            while *data.get(i)? != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while data.get(i)?.is_ascii_digit() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&data[start..i]).ok()?.parse::<i32>().ok()?);
    }
    let (w, h) = (fields[0], fields[1]);
    let body = i + 1;
    (fields[2] == 255 && data.len() >= body + (w * h * 3) as usize).then(|| (w, h, data[body..body + (w * h * 3) as usize].to_vec()))
}

fn mon(c: Captured) -> Option<Mon> {
    let (w, h) = (c.width, c.height);
    let mut image = cairo::ImageSurface::create(cairo::Format::Rgb24, w, h).ok()?;
    let stride = image.stride() as usize;
    {
        let mut data = image.data().ok()?;
        for y in 0..h as usize {
            let (src, dst) = (&c.rgb[y * w as usize * 3..][..w as usize * 3], &mut data[y * stride..][..w as usize * 4]);
            for (s, d) in src.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 0xff]);
            }
        }
    }
    let bytes = glib::Bytes::from_owned(c.rgb);
    let texture = gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::R8g8b8, &bytes, w as usize * 3);
    let area = Rect { x: c.x, y: c.y, w: (f64::from(w) / c.scale).round(), h: (f64::from(h) / c.scale).round() };
    Some(Mon { name: c.name, area, scale: c.scale, image, texture })
}

// ---- the overlays -------------------------------------------------------------------

impl Shot {
    fn open(self: &Rc<Self>, app: &gtk::Application) {
        let monitors: Vec<gdk::Monitor> = gdk::Display::default().map(|d| d.monitors().iter::<gdk::Monitor>().filter_map(Result::ok).collect()).unwrap_or_default();
        let mut overlays = Vec::new();
        for (i, mon) in self.mons.iter().enumerate() {
            let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
            window.init_layer_shell();
            window.set_namespace(Some("eclipse-shell-screenshot"));
            window.set_layer(Layer::Overlay);
            window.set_keyboard_mode(KeyboardMode::Exclusive);
            window.set_exclusive_zone(-1);
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
            window.set_monitor(monitors.iter().find(|m| m.connector().as_deref() == Some(mon.name.as_str())));

            let picture = gtk::Picture::for_paintable(&mon.texture);
            picture.set_content_fit(gtk::ContentFit::Fill);
            picture.set_can_shrink(true);
            let fixed = gtk::Fixed::new();
            let dims = std::array::from_fn(|_| {
                let d = gtk::DrawingArea::new();
                d.set_draw_func(|_, cr, _, _| {
                    theme::set_source(cr, DIM);
                    let _ = cr.paint();
                });
                d.set_can_target(false);
                fixed.put(&d, 0.0, 0.0);
                d
            });
            let hover = gtk::DrawingArea::new();
            hover.set_can_target(false);
            hover.set_draw_func(|_, cr, w, h| {
                let (w, h) = (f64::from(w), f64::from(h));
                cr.rectangle(1.25, 1.25, w - 2.5, h - 2.5);
                theme::set_source(cr, Rgba { r: 0.88, g: 0.34, b: 0.23, a: 0.16 });
                let _ = cr.fill_preserve();
                cr.set_line_width(2.5);
                theme::set_source(cr, theme::palette().accent_lit);
                let _ = cr.stroke();
            });
            fixed.put(&hover, 0.0, 0.0);
            let area = gtk::DrawingArea::new();
            area.set_can_target(false);
            fixed.put(&area, 0.0, 0.0);
            let weak = Rc::downgrade(self);
            let toolbar = Toolbar::new(
                self.view(),
                Rc::new(move |a| {
                    if let Some(shot) = weak.upgrade() {
                        shot.action(a);
                    }
                }),
                &fixed,
            );
            fixed.put(&toolbar.widget, 0.0, 0.0);
            fixed.put(&toolbar.panel, 0.0, 0.0);
            fixed.put(&toolbar.tip, 0.0, 0.0);
            toolbar.widget.set_visible(false);

            let stage = gtk::Overlay::new();
            stage.set_child(Some(&picture));
            stage.add_overlay(&fixed);
            stage.set_cursor_from_name(Some("crosshair"));
            toolbar.widget.set_cursor_from_name(Some("default"));
            toolbar.panel.set_cursor_from_name(Some("default"));
            window.set_child(Some(&stage));
            self.wire(i, &window, &stage, &fixed);
            overlays.push(Overlay { window, mon: i, fixed, dims, area, area_at: Cell::new((0.0, 0.0)), hover, toolbar });
        }
        *self.overlays.borrow_mut() = overlays;
        self.layout();
        for o in self.overlays.borrow().iter() {
            let this = Rc::downgrade(self);
            o.area.set_draw_func(move |area, cr, _, _| {
                if let Some(shot) = this.upgrade() {
                    shot.draw_area(area, cr);
                }
            });
            o.window.present();
        }
    }

    fn wire(self: &Rc<Self>, mon: usize, window: &gtk::ApplicationWindow, stage: &gtk::Overlay, fixed: &gtk::Fixed) {
        let origin = (self.mons[mon].area.x, self.mons[mon].area.y);
        let on_bar = {
            let fixed = fixed.downgrade();
            move |x: f64, y: f64| {
                // A press on the toolbar or its panel is theirs.
                let Some(fixed) = fixed.upgrade() else { return false };
                fixed.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| w != fixed.clone().upcast::<gtk::Widget>())
            }
        };
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let start = Rc::new(Cell::new(None::<(f64, f64)>));
        drag.connect_drag_begin({
            let (this, start) = (Rc::downgrade(self), start.clone());
            move |_, x, y| {
                if on_bar(x, y) {
                    start.set(None);
                    return;
                }
                start.set(Some((x, y)));
                if let Some(s) = this.upgrade() {
                    s.pressed(origin.0 + x, origin.1 + y);
                }
            }
        });
        drag.connect_drag_update({
            let (this, start) = (Rc::downgrade(self), start.clone());
            move |_, dx, dy| {
                if let (Some(s), Some((x, y))) = (this.upgrade(), start.get()) {
                    s.moved(origin.0 + x + dx, origin.1 + y + dy);
                }
            }
        });
        drag.connect_drag_end({
            let (this, start) = (Rc::downgrade(self), start);
            move |_, _, _| {
                if let (Some(s), Some(_)) = (this.upgrade(), start.take()) {
                    s.released();
                }
            }
        });
        stage.add_controller(drag);
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let this = Rc::downgrade(self);
            move |_, x, y| {
                if let Some(s) = this.upgrade() {
                    s.hovered(origin.0 + x, origin.1 + y);
                }
            }
        });
        stage.add_controller(motion);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let this = Rc::downgrade(self);
            move |_, key, _, mods| match this.upgrade() {
                Some(s) if s.key(key, mods) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(keys);
    }

    fn view(&self) -> View {
        let st = self.state.borrow();
        View {
            tool: st.tool,
            color: st.color,
            width: st.width,
            rough: st.rough,
            can_undo: st.model.can_undo(),
            can_redo: st.model.can_redo(),
            settings_open: st.settings_open,
            hotkey: st.hotkey.clone(),
            listening: st.listening,
        }
    }

    /// The overlay the toolbar belongs on: the one holding the selection's corner.
    fn anchor(&self) -> Option<usize> {
        let sel = self.state.borrow().sel?;
        self.mons.iter().position(|m| m.area.contains(sel.x, sel.y)).or(Some(0))
    }

    /// Places everything for the current state, and redraws.
    fn layout(&self) {
        let st = self.state.borrow();
        let anchor = self.anchor();
        let view = self.view_from(&st);
        for o in self.overlays.borrow().iter() {
            let m = &self.mons[o.mon].area;
            let local = st.sel.and_then(|s| s.intersect(m)).map(|r| Rect { x: r.x - m.x, y: r.y - m.y, ..r });
            // The dim, around the selection or over everything.
            let boxes = match local {
                Some(l) => [(0.0, 0.0, m.w, l.y), (0.0, l.y + l.h, m.w, m.h - l.y - l.h), (0.0, l.y, l.x, l.h), (l.x + l.w, l.y, m.w - l.x - l.w, l.h)],
                None if st.sel.is_some() => [(0.0, 0.0, m.w, m.h), (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.0)],
                None => [(0.0, 0.0, m.w, m.h), (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.0)],
            };
            for (d, (x, y, w, h)) in o.dims.iter().zip(boxes) {
                place(&o.fixed, d, x, y, w, h);
            }
            match local {
                Some(l) => {
                    let (x0, y0) = ((l.x - PAD).max(0.0), (l.y - PAD).max(0.0));
                    let (x1, y1) = ((l.x + l.w + PAD).min(m.w), (l.y + l.h + PAD).min(m.h));
                    place(&o.fixed, &o.area, x0, y0, x1 - x0, y1 - y0);
                    o.area_at.set((x0, y0));
                    o.area.set_visible(true);
                }
                None => o.area.set_visible(false),
            }
            match st.hover.filter(|_| st.sel.is_none()).and_then(|h| h.intersect(m)) {
                Some(h) => {
                    place(&o.fixed, &o.hover, h.x - m.x, h.y - m.y, h.w, h.h);
                    o.hover.set_visible(true);
                }
                None => o.hover.set_visible(false),
            }
            // The toolbar: under the selection, or over it with no room below.
            let show = st.phase == Phase::Editing && anchor == Some(o.mon) && local.is_some();
            o.toolbar.widget.set_visible(show);
            o.toolbar.update(view.clone());
            if let (true, Some(l)) = (show, local) {
                let (_, nat) = o.toolbar.widget.preferred_size();
                let (tw, th) = (f64::from(nat.width()), f64::from(nat.height()));
                let x = (l.x + l.w / 2.0 - tw / 2.0).clamp(8.0, (m.w - tw - 8.0).max(8.0));
                let mut y = l.y + l.h + 12.0;
                if y + th > m.h - 8.0 {
                    y = l.y - th - 12.0;
                }
                let y = y.max(8.0);
                o.fixed.move_(&o.toolbar.widget, x, y);
                let (_, pnat) = o.toolbar.panel.preferred_size();
                let px = (x + o.toolbar.gear_x() - f64::from(pnat.width()) / 2.0).clamp(8.0, (m.w - f64::from(pnat.width()) - 8.0).max(8.0));
                o.fixed.move_(&o.toolbar.panel, px, (y - f64::from(pnat.height()) - 6.0).max(8.0));
            }
            o.area.queue_draw();
        }
    }

    fn view_from(&self, st: &State) -> View {
        View {
            tool: st.tool,
            color: st.color,
            width: st.width,
            rough: st.rough,
            can_undo: st.model.can_undo(),
            can_redo: st.model.can_redo(),
            settings_open: st.settings_open,
            hotkey: st.hotkey.clone(),
            listening: st.listening,
        }
    }

    /// Redraws the selection areas only, for changes inside the selection.
    fn redraw(&self) {
        self.overlays.borrow().iter().for_each(|o| o.area.queue_draw());
    }

    /// The annotations as drawn now: a moved one where it's being dragged,
    /// and the one being drawn last.
    fn items(&self, st: &State) -> Vec<Ann> {
        let mut items = st.model.items.clone();
        if let Some(i) = st.selected.filter(|&i| i < items.len()) {
            items[i] = items[i].shifted(st.move_offset.0, st.move_offset.1);
        }
        items.extend(st.draft.clone());
        items
    }

    fn draw_area(&self, area: &gtk::DrawingArea, cr: &cairo::Context) {
        let overlays = self.overlays.borrow();
        let Some(o) = overlays.iter().find(|o| &o.area == area) else { return };
        let st = self.state.borrow();
        let Some(sel) = st.sel else { return };
        let mon = &self.mons[o.mon];
        let (ax, ay) = o.area_at.get();
        // Global logical coordinates from here on.
        cr.translate(-(mon.area.x + ax), -(mon.area.y + ay));
        let _ = cr.save();
        cr.rectangle(sel.x, sel.y, sel.w, sel.h);
        cr.clip();
        let items = self.items(&st);
        for a in items.iter().filter(|a| a.kind.is_effect()) {
            ann::draw_effect(cr, a, &mon.image, mon.area.x, mon.area.y, mon.scale);
        }
        let typing = st.text_editing;
        let last = items.len().saturating_sub(1);
        for (i, a) in items.iter().enumerate().filter(|(_, a)| !a.kind.is_effect()) {
            ann::draw(cr, a, typing && i == last && st.draft.is_some());
        }
        let _ = cr.restore();

        // The chrome: outline, corner handles, and the size above.
        let accent = theme::palette().accent_lit;
        theme::set_source(cr, accent);
        cr.set_line_width(1.5);
        cr.rectangle(sel.x, sel.y, sel.w, sel.h);
        let _ = cr.stroke();
        for (hx, hy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            cr.rectangle(sel.x + hx * (sel.w - 8.0), sel.y + hy * (sel.h - 8.0), 8.0, 8.0);
        }
        let _ = cr.fill();
        let label = format!("eclipse \u{b7} {}\u{d7}{}", sel.w.round(), sel.h.round());
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&TextStyle::new(13.0, pango::Weight::Medium, accent).mono().font()));
        layout.set_text(&label);
        let (_, logical) = layout.pixel_extents();
        cr.move_to(sel.x, sel.y - f64::from(logical.height()) - 4.0);
        pangocairo::functions::show_layout(cr, &layout);

        // The selected annotation's box.
        if let Some(a) = st.selected.and_then(|i| items.get(i)) {
            let (bx, by, bw, bh) = a.bbox();
            let pad = if a.kind == Kind::Text { 4.0 } else { a.width.max(6.0) };
            let (x, y, w, h) = (bx - pad, by - pad, bw + 2.0 * pad, bh + 2.0 * pad);
            cr.set_line_width(1.0);
            cr.rectangle(x, y, w, h);
            let _ = cr.stroke();
            for (hx, hy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                theme::rounded_rect(cr, x + hx * (w - 7.0), y + hy * (h - 7.0), 7.0, 7.0, 1.0);
                let _ = cr.fill();
            }
        }
    }

    // ---- input ------------------------------------------------------------------

    fn window_at(&self, x: f64, y: f64) -> Option<Rect> {
        if self.monitor_mode {
            return self.mons.iter().find(|m| m.area.contains(x, y)).map(|m| m.area);
        }
        self.windows.iter().filter(|(r, _)| r.contains(x, y)).min_by_key(|(_, z)| *z).map(|(r, _)| *r)
    }

    fn hovered(&self, x: f64, y: f64) {
        let hover = {
            let st = self.state.borrow();
            if st.phase != Phase::Selecting || st.dragging || st.busy {
                return;
            }
            self.window_at(x, y)
        };
        if self.state.borrow().hover != hover {
            self.state.borrow_mut().hover = hover;
            self.layout();
        }
    }

    fn pressed(&self, x: f64, y: f64) {
        let mut st = self.state.borrow_mut();
        if st.busy {
            return;
        }
        st.dragging = true;
        match st.phase {
            Phase::Selecting if self.monitor_mode => {
                if let Some(m) = self.mons.iter().find(|m| m.area.contains(x, y)) {
                    st.sel = Some(m.area);
                    st.phase = Phase::Editing;
                    st.hover = None;
                }
            }
            Phase::Selecting => {
                st.press = Some((x, y));
                st.sel = Some(Rect { x, y, w: 0.0, h: 0.0 });
            }
            Phase::Editing if st.tool == "select" => {
                st.selected = st.model.hit(x, y);
                if st.selected.is_some() {
                    st.move_start = Some((x, y));
                    st.move_offset = (0.0, 0.0);
                }
            }
            Phase::Editing => {
                let Some(sel) = st.sel else { return };
                let p = sel.clamp(x, y);
                let (color, width) = (st.color, st.width);
                match st.tool {
                    "text" => {
                        if st.text_editing {
                            drop(st);
                            self.commit_text();
                            return;
                        }
                        let mut a = Ann::new(Kind::Text, vec![p], color, width);
                        a.size = width * 5.0 + 8.0;
                        st.draft = Some(a);
                        st.text_editing = true;
                    }
                    "counter" => {
                        let mut a = Ann::new(Kind::Counter, vec![p], color, width);
                        a.n = st.model.counters() + 1;
                        st.model.add(a);
                    }
                    tool => {
                        st.press = Some(p);
                        let kind = kind_of(tool);
                        st.draft = Some(match kind {
                            Kind::Pen => Ann::new(kind, vec![p], color, width),
                            Kind::Marker => Ann::new(kind, vec![p, p], ann::MARKER, width),
                            _ => {
                                let mut a = Ann::new(kind, vec![p, p], color, width);
                                a.rough = st.rough && kind.roughable();
                                a
                            }
                        });
                    }
                }
            }
        }
        drop(st);
        self.layout();
    }

    fn moved(&self, x: f64, y: f64) {
        let mut st = self.state.borrow_mut();
        if !st.dragging || st.busy {
            return;
        }
        match st.phase {
            Phase::Selecting => {
                if let Some(p) = st.press {
                    st.sel = Some(Rect::from_points(p, (x, y)));
                    drop(st);
                    self.layout();
                }
            }
            Phase::Editing if st.tool == "select" => {
                if let (Some(_), Some(start)) = (st.selected, st.move_start) {
                    st.move_offset = (x - start.0, y - start.1);
                    drop(st);
                    self.redraw();
                }
            }
            Phase::Editing => {
                let (Some(sel), Some(press)) = (st.sel, st.press) else { return };
                let p = sel.clamp(x, y);
                let Some(draft) = st.draft.as_mut() else { return };
                if draft.kind == Kind::Pen {
                    let last = draft.points[draft.points.len() - 1];
                    if (p.0 - last.0).abs() < 2.0 && (p.1 - last.1).abs() < 2.0 {
                        return;
                    }
                    draft.points.push(p);
                } else if draft.kind != Kind::Text {
                    draft.points = vec![press, p];
                }
                drop(st);
                self.redraw();
            }
        }
    }

    fn released(&self) {
        let mut st = self.state.borrow_mut();
        if !st.dragging {
            return;
        }
        st.dragging = false;
        match st.phase {
            Phase::Selecting => {
                st.press = None;
                if st.sel.is_some_and(|s| s.w > 2.0 && s.h > 2.0) {
                    st.phase = Phase::Editing;
                    st.hover = None;
                } else if let Some(h) = st.hover.take() {
                    // A click picks the window under it.
                    st.sel = Some(h);
                    st.phase = Phase::Editing;
                } else {
                    st.sel = None;
                }
            }
            Phase::Editing if st.tool == "select" => {
                if let Some(i) = st.selected {
                    let (dx, dy) = std::mem::take(&mut st.move_offset);
                    if dx != 0.0 || dy != 0.0 {
                        st.model.shift(i, dx, dy);
                    }
                }
                st.move_start = None;
            }
            Phase::Editing => {
                st.press = None;
                if let Some(d) = st.draft.take_if(|d| d.kind != Kind::Text) {
                    let keep = match d.kind {
                        Kind::Pen => d.points.len() >= 2,
                        Kind::Line | Kind::Arrow => {
                            let (p0, p1) = (d.points[0], d.points[1]);
                            (p1.0 - p0.0).hypot(p1.1 - p0.1) > 4.0
                        }
                        _ => {
                            let (_, _, w, h) = d.rect();
                            w > 2.0 && h > 2.0
                        }
                    };
                    if keep {
                        st.model.add(d);
                    }
                }
            }
        }
        drop(st);
        self.layout();
    }

    fn commit_text(&self) {
        let mut st = self.state.borrow_mut();
        if let Some(d) = st.draft.take_if(|d| d.kind == Kind::Text) {
            if !d.text.is_empty() {
                st.model.add(d);
            }
        }
        st.text_editing = false;
        drop(st);
        self.layout();
    }

    fn cancel_text(&self) {
        let mut st = self.state.borrow_mut();
        st.draft = None;
        st.text_editing = false;
        drop(st);
        self.redraw();
    }

    /// A key on any overlay; true when it was used.
    fn key(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> bool {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let (typing, listening, settings, selected, busy) = {
            let st = self.state.borrow();
            (st.text_editing, st.listening, st.settings_open, st.selected, st.busy)
        };
        if busy {
            return true;
        }
        if listening {
            if key == gdk::Key::Escape {
                self.state.borrow_mut().listening = false;
                self.layout();
            } else if let Some(bind) = hotkey::bind_string(key, mods) {
                hotkey::set(&bind);
                {
                    let mut st = self.state.borrow_mut();
                    st.listening = false;
                    st.hotkey = bind;
                }
                self.layout();
            }
            return true;
        }
        if typing {
            match key {
                gdk::Key::Escape => self.cancel_text(),
                gdk::Key::Return | gdk::Key::KP_Enter => self.commit_text(),
                gdk::Key::BackSpace => {
                    if let Some(d) = self.state.borrow_mut().draft.as_mut() {
                        d.text.pop();
                    }
                    self.redraw();
                }
                _ if !ctrl => {
                    if let Some(c) = key.to_unicode().filter(|c| !c.is_control()) {
                        if let Some(d) = self.state.borrow_mut().draft.as_mut() {
                            d.text.push(c);
                        }
                        self.redraw();
                    }
                }
                _ => {}
            }
            return true;
        }
        match key {
            gdk::Key::Escape => {
                if settings {
                    self.state.borrow_mut().settings_open = false;
                    self.layout();
                } else if selected.is_some() {
                    self.state.borrow_mut().selected = None;
                    self.redraw();
                } else {
                    self.close();
                }
            }
            gdk::Key::c | gdk::Key::C if ctrl => self.action(Action::Copy),
            gdk::Key::z | gdk::Key::Z if ctrl => self.action(Action::Undo),
            gdk::Key::y | gdk::Key::Y if ctrl => self.action(Action::Redo),
            gdk::Key::Delete | gdk::Key::BackSpace if selected.is_some() => {
                let mut st = self.state.borrow_mut();
                if let Some(i) = st.selected.take() {
                    st.model.remove(i);
                }
                drop(st);
                self.layout();
            }
            _ => return false,
        }
        true
    }

    fn action(self: &Rc<Self>, action: Action) {
        if self.state.borrow().busy {
            return;
        }
        if self.state.borrow().text_editing && !matches!(action, Action::Color(_) | Action::Width(_)) {
            self.commit_text();
        }
        {
            let mut st = self.state.borrow_mut();
            match &action {
                Action::Tool(t) => {
                    st.tool = t;
                    st.selected = None;
                }
                Action::Color(c) => st.color = *c,
                Action::Width(w) => st.width = *w,
                Action::Rough => st.rough = !st.rough,
                Action::Undo | Action::Redo => {
                    let done = if matches!(action, Action::Undo) { st.model.undo() } else { st.model.redo() };
                    if done {
                        st.selected = None;
                        st.move_start = None;
                        st.move_offset = (0.0, 0.0);
                    }
                }
                Action::Settings => {
                    st.settings_open = !st.settings_open;
                    st.listening = false;
                }
                Action::Listen => st.listening = !st.listening,
                Action::Copy | Action::Save | Action::Upload => {}
            }
        }
        match action {
            Action::Copy => self.copy(),
            Action::Save => self.save(),
            Action::Upload => self.upload(),
            _ => self.layout(),
        }
    }

    // ---- export -----------------------------------------------------------------

    /// The selection with its annotations, at the sharpest scale it spans.
    fn render(&self) -> Option<cairo::ImageSurface> {
        let st = self.state.borrow();
        let sel = st.sel?;
        let scale = self.mons.iter().filter(|m| m.area.intersect(&sel).is_some()).map(|m| m.scale).fold(1.0, f64::max);
        let (w, h) = ((sel.w * scale).round() as i32, (sel.h * scale).round() as i32);
        let base = cairo::ImageSurface::create(cairo::Format::Rgb24, w.max(1), h.max(1)).ok()?;
        {
            // The frozen screens, a slice from each monitor the selection crosses.
            let cr = cairo::Context::new(&base).ok()?;
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.paint().ok()?;
            for m in &self.mons {
                let Some(part) = m.area.intersect(&sel) else { continue };
                let _ = cr.save();
                cr.scale(scale, scale);
                cr.translate(-sel.x, -sel.y);
                cr.rectangle(part.x, part.y, part.w, part.h);
                cr.clip();
                cr.translate(m.area.x, m.area.y);
                cr.scale(1.0 / m.scale, 1.0 / m.scale);
                cr.set_source_surface(&m.image, 0.0, 0.0).ok()?;
                if m.scale != scale {
                    cr.source().set_filter(cairo::Filter::Good);
                }
                cr.paint().ok()?;
                let _ = cr.restore();
            }
        }
        base.flush();
        let out = cairo::ImageSurface::create(cairo::Format::Rgb24, w.max(1), h.max(1)).ok()?;
        {
            let cr = cairo::Context::new(&out).ok()?;
            cr.set_source_surface(&base, 0.0, 0.0).ok()?;
            cr.paint().ok()?;
            cr.scale(scale, scale);
            cr.translate(-sel.x, -sel.y);
            let items = self.items(&st);
            // Effects read the untouched picture, as ryoshot's did.
            for a in items.iter().filter(|a| a.kind.is_effect()) {
                ann::draw_effect(&cr, a, &base, sel.x, sel.y, scale);
            }
            for a in items.iter().filter(|a| !a.kind.is_effect()) {
                ann::draw(&cr, a, false);
            }
        }
        out.flush();
        Some(out)
    }

    /// Renders, then hands the pixels to a worker thread (cairo surfaces
    /// can't cross threads) that encodes the PNG.
    fn export(&self) -> Option<async_channel::Receiver<Option<Vec<u8>>>> {
        {
            let mut st = self.state.borrow_mut();
            st.busy = true;
            st.selected = None;
        }
        let mut surface = self.render()?;
        let (w, h, stride) = (surface.width(), surface.height(), surface.stride() as usize);
        let pixels = surface.data().ok()?.to_vec();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(encode_png(&pixels, w, h, stride));
        });
        Some(rx)
    }

    fn hide(&self) {
        self.overlays.borrow().iter().for_each(|o| o.window.set_visible(false));
    }

    fn close(&self) {
        for o in self.overlays.borrow_mut().drain(..) {
            o.window.destroy();
        }
        OPEN.with_borrow_mut(|o| *o = None);
    }

    fn copy(self: &Rc<Self>) {
        let Some(png) = self.export() else { return self.close() };
        // Nothing is left to show: go now, and finish behind.
        self.hide();
        let this = self.clone();
        glib::spawn_future_local(async move {
            this.close();
            let Ok(Some(png)) = png.recv().await else { return notify("Screenshot failed", "Couldn't encode the picture.") };
            let path = default_path();
            if let Err(e) = write_file(&path, &png) {
                eprintln!("eclipse-shell screenshot: {}: {e}", path.display());
            }
            if gio::spawn_blocking(|| crate::deps::ensure("wl-copy", "wl-clipboard")).await.unwrap_or(false) {
                clipboard(&png, "image/png");
            }
        });
    }

    fn save(self: &Rc<Self>) {
        let Some(png) = self.export() else { return self.close() };
        self.hide();
        // The encode runs while the dialog is open.
        let png = Rc::new(RefCell::new(Some(png)));
        let path = default_path();
        let dialog = gtk::FileDialog::builder().title("Save screenshot").modal(true).build();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        dialog.set_initial_name(path.file_name().and_then(|n| n.to_str()));
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("PNG"));
        filter.add_pattern("*.png");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        let this = self.clone();
        dialog.save(None::<&gtk::Window>, None::<&gio::Cancellable>, move |chosen| {
            let Ok(Some(dest)) = chosen.map(|f| f.path()) else {
                // Cancelled: back to the overlay, as ryoshot did.
                this.state.borrow_mut().busy = false;
                this.overlays.borrow().iter().for_each(|o| o.window.set_visible(true));
                return;
            };
            this.close();
            let Some(png) = png.borrow_mut().take() else { return };
            glib::spawn_future_local(async move {
                match png.recv().await {
                    Ok(Some(png)) => {
                        if let Err(e) = write_file(&dest, &png) {
                            notify("Screenshot not saved", &format!("{}: {e}", dest.display()));
                        }
                    }
                    _ => notify("Screenshot failed", "Couldn't encode the picture."),
                }
            });
        });
    }

    fn upload(self: &Rc<Self>) {
        let Some(png) = self.export() else { return self.close() };
        self.hide();
        let this = self.clone();
        glib::spawn_future_local(async move {
            this.close();
            let Ok(Some(png)) = png.recv().await else { return notify("Upload failed", "Couldn't prepare the image for upload.") };
            let result = gio::spawn_blocking(move || upload(&png)).await;
            match result {
                Ok(Ok(url)) => {
                    if gio::spawn_blocking(|| crate::deps::ensure("wl-copy", "wl-clipboard")).await.unwrap_or(false) {
                        clipboard(url.as_bytes(), "text/plain;charset=utf-8");
                    }
                    notify("Screenshot uploaded", &format!("Link copied: {url} (expires in 3 days)"));
                }
                Ok(Err(e)) => notify("Upload failed", &e),
                Err(_) => notify("Upload failed", "The upload stopped unexpectedly."),
            }
        });
    }
}

fn kind_of(tool: &str) -> Kind {
    match tool {
        "ellipse" => Kind::Ellipse,
        "line" => Kind::Line,
        "arrow" => Kind::Arrow,
        "pen" => Kind::Pen,
        "marker" => Kind::Marker,
        "text" => Kind::Text,
        "blur" => Kind::Blur,
        "pixelate" => Kind::Pixelate,
        "magnify" => Kind::Magnify,
        "counter" => Kind::Counter,
        _ => Kind::Rect,
    }
}

fn place(fixed: &gtk::Fixed, w: &impl IsA<gtk::Widget>, x: f64, y: f64, width: f64, height: f64) {
    w.set_size_request(width.max(0.0).round() as i32, height.max(0.0).round() as i32);
    w.set_visible(width > 0.0 && height > 0.0);
    fixed.move_(w, x.round(), y.round());
}

/// PNG bytes from cairo RGB24 pixels, opaque. zlib level 3 without row
/// filters (gdk-pixbuf's PNG writer filters by default only at higher
/// levels): ryoshot's measured trade, about as small and several times faster
/// than the default.
fn encode_png(pixels: &[u8], w: i32, h: i32, stride: usize) -> Option<Vec<u8>> {
    let started = std::time::Instant::now();
    let (wu, hu) = (w as usize, h as usize);
    let mut rgb = Vec::with_capacity(wu * hu * 3);
    for y in 0..hu {
        for px in pixels[y * stride..][..wu * 4].chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(rgb), gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, w, h, w * 3);
    let png = pixbuf.save_to_bufferv("png", &[("compression", "3")]).ok();
    if std::env::var_os("ECLIPSE_SHOT_TIMING").is_some() {
        eprintln!("eclipse-shell screenshot: {w}x{h} PNG in {:?}, {} bytes", started.elapsed(), png.as_ref().map_or(0, Vec::len));
    }
    png
}

fn shots_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(|| glib::home_dir().join("Pictures")).join("Screenshots")
}

fn default_path() -> PathBuf {
    let stamp = glib::DateTime::now_local().and_then(|d| d.format("%Y%m%d-%H%M%S")).map(|s| s.to_string()).unwrap_or_default();
    shots_dir().join(format!("shot-{stamp}.png"))
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// Puts `bytes` on the clipboard through wl-copy, in a process group of its
/// own so nothing that ends here takes it down: wl-copy forks a server that
/// must live until a paste.
fn clipboard(bytes: &[u8], mime: &str) {
    use std::os::unix::process::CommandExt;
    let child = Command::new("wl-copy").args(["--type", mime]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn();
    match child {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(bytes);
            }
            // The foreground wl-copy exits once its server has the data.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => notify("Copy failed", &format!("Couldn't run wl-copy: {e}")),
    }
}

/// Uploads to litterbox (72 hours); the link, or why not.
fn upload(png: &[u8]) -> Result<String, String> {
    let file = std::env::temp_dir().join(format!("eclipse-shot-upload-{}.png", std::process::id()));
    std::fs::write(&file, png).map_err(|_| "Couldn't prepare the image for upload.".to_owned())?;
    // No -f, so a rejected upload still gives the host's status and reply;
    // the status comes on the last line.
    let out = Command::new("curl")
        .args(["-s", "--max-time", "30", "-A", "Mozilla/5.0", "-w", "\n%{http_code}", "-F", "reqtype=fileupload", "-F", "time=72h", "-F"])
        .arg(format!("fileToUpload=@{}", file.display()))
        .arg("https://litterbox.catbox.moe/resources/internals/api.php")
        .output();
    let _ = std::fs::remove_file(&file);
    let out = out.map_err(|e| format!("Couldn't run curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let (body, status) = text.rsplit_once('\n').map_or(("", text.as_str()), |(b, s)| (b.trim(), s.trim()));
    let code = out.status.code().unwrap_or(-1);
    if code == 0 && status == "200" && body.starts_with("http") {
        return Ok(body.to_owned());
    }
    // A plain-text reply is the host's own message; HTML is a proxy page.
    Err(match code {
        6 => "No connection: couldn't reach the upload host.".to_owned(),
        28 => "The upload timed out after 30 seconds.".to_owned(),
        0 if body.starts_with('<') => format!("The upload host blocked the request (HTTP {status})."),
        0 => {
            let msg = if !body.is_empty() && body.len() <= 120 { format!(": {body}") } else { String::new() };
            if status == "200" { format!("The upload host sent an unexpected reply{msg}") } else { format!("The upload host returned HTTP {status}{msg}") }
        }
        _ => format!("Couldn't connect to the upload host (curl error {code})."),
    })
}

fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send").args(["-a", "Screenshot", "-i", "camera-photo", title, body]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|mut c| {
        std::thread::spawn(move || {
            let _ = c.wait();
        })
    });
}

/// The capture keys, in ~/.config/hypr/eclipse_capture.lua: the gear's
/// panel shows and records the region key (the first bind there).
mod hotkey {
    use super::*;

    pub fn path() -> PathBuf {
        glib::user_config_dir().join("hypr").join("eclipse_capture.lua")
    }

    /// The first bind's keys, as Hyprland spells them.
    pub fn current() -> Option<String> {
        let text = std::fs::read_to_string(path()).ok()?;
        text.lines().find_map(|l| l.trim().strip_prefix("hl.bind(\"").and_then(|r| r.split_once('"')).map(|(k, _)| k.to_owned()))
    }

    /// Rebinds the region key and reloads Hyprland.
    pub fn set(bind: &str) {
        let Ok(text) = std::fs::read_to_string(path()) else { return };
        let mut done = false;
        let out: String = text
            .lines()
            .map(|l| match l.trim().strip_prefix("hl.bind(\"").and_then(|r| r.split_once('"')) {
                Some((_, rest)) if !done => {
                    done = true;
                    format!("hl.bind(\"{bind}\"{rest}\n")
                }
                _ => format!("{l}\n"),
            })
            .collect();
        if std::fs::write(path(), out).is_ok() {
            let _ = hyprland::request("reload");
        }
    }

    /// A key press as a bind, "SUPER + SHIFT + s"; none for a lone modifier.
    pub fn bind_string(key: gdk::Key, mods: gdk::ModifierType) -> Option<String> {
        use gdk::ModifierType as M;
        let name = key.name()?.to_string();
        if ["Shift", "Control", "Alt", "Super", "Meta", "Hyper", "ISO_Level3_Shift", "Caps_Lock"].iter().any(|m| name.starts_with(m)) {
            return None;
        }
        let name = if name.chars().count() == 1 { name.to_lowercase() } else { name };
        let mut parts: Vec<String> = [(M::SUPER_MASK, "SUPER"), (M::CONTROL_MASK, "CTRL"), (M::ALT_MASK, "ALT"), (M::SHIFT_MASK, "SHIFT")]
            .into_iter()
            .filter(|(m, _)| mods.contains(*m))
            .map(|(_, n)| n.to_owned())
            .collect();
        parts.push(name);
        Some(parts.join(" + "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm() {
        let mut data = b"P6\n# grim\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(parse_ppm(data), Some((2, 1, vec![1, 2, 3, 4, 5, 6])));
        assert_eq!(parse_ppm(b"P5\n1 1\n255\n\0".to_vec()), None);
    }

    #[test]
    fn rects() {
        let a = Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 };
        let b = Rect { x: 80.0, y: 40.0, w: 100.0, h: 100.0 };
        assert_eq!(a.intersect(&b), Some(Rect { x: 80.0, y: 40.0, w: 20.0, h: 10.0 }));
        assert_eq!(Rect::from_points((10.0, 5.0), (2.0, 9.0)), Rect { x: 2.0, y: 5.0, w: 8.0, h: 4.0 });
        assert_eq!(a.clamp(-5.0, 70.0), (0.0, 50.0));
    }
}
