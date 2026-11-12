//! Overview: every workspace on the focused screen at once, over the blurred
//! desktop. Each is the wallpaper with its windows as live pictures where
//! they sit (src/capture.rs, four times a second while open), and a "+" for a
//! fresh workspace. Click a workspace to go there, click a window to focus
//! it, drag a window onto another workspace to move it, hover a window for a
//! button that closes it. Arrows, Tab or scrolling move the selection, Enter
//! goes there, Escape or a click on the backdrop closes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, graphene, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::anim::{self, Easing, Tween, MORPH};
use crate::capture::{self, Job};
use crate::hint;
use crate::hyprland::{self, Layout, Placed, Screen};
use crate::popout::{self, on_hover};
use crate::theme::{self, TextStyle, BRIGHT, CREAM, SUBTLE};
use crate::{config, icons, wallpaper};

const NAMESPACE: &str = "eclipse-shell-overview";
const FADE_MS: f64 = 180.0;
/// How often the pictures refresh: four times a second.
const FRAME: Duration = Duration::from_millis(250);
/// How far a press must travel before it's a drag rather than a click.
const DRAG_START: f64 = 6.0;
/// Hyprland takes a moment to settle a move before the layout is read again.
const SETTLE: Duration = Duration::from_millis(150);
/// Decode the wallpaper at about this many device pixels tall.
const WALLPAPER_HEIGHT: i32 = 540;
/// The "+" slat's width, as a share of a workspace's.
const SLAT: f64 = 0.3;

/// A workspace on the grid (or the "+"), as a drop target and selection stop.
struct Slot {
    id: i32,
    widget: gtk::Widget,
    back: gtk::DrawingArea,
}

/// A window's picture and the icon it shows until the first one arrives.
struct Tile {
    area: gtk::DrawingArea,
    icon: gtk::Image,
}

pub struct Overview {
    window: gtk::ApplicationWindow,
    layers: gtk::Overlay,
    stage: gtk::Fixed,
    hints: gtk::Fixed,
    fade: Rc<RefCell<Tween>>,
    open: Cell<bool>,
    /// Bumped on every open, so a stale picture ticker stops.
    generation: Cell<u32>,
    monitor: RefCell<Option<gdk::Monitor>>,
    layout: RefCell<Option<(Layout, Screen)>>,
    slots: RefCell<Vec<Slot>>,
    /// Columns in the grid, for moving the selection up and down.
    columns: Cell<usize>,
    selected: Rc<Cell<usize>>,
    /// The cell a carried window would drop on.
    drop_hot: Rc<Cell<Option<usize>>>,
    pictures: Rc<RefCell<HashMap<String, cairo::ImageSurface>>>,
    tiles: RefCell<HashMap<String, Tile>>,
    /// Device px wide to capture each window at.
    capture_widths: RefCell<HashMap<String, u32>>,
    wallpaper: Rc<RefCell<Option<cairo::ImageSurface>>>,
    wallpaper_path: RefCell<String>,
}

impl Overview {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some(NAMESPACE));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        hyprland::keep(|| hyprland::eval(&format!("hl.layer_rule({{ name = \"{NAMESPACE}-blur\", match = {{ namespace = \"{NAMESPACE}\" }}, blur = true, no_anim = true }})")));

        let backdrop = gtk::DrawingArea::new();
        backdrop.set_draw_func(|_, cr, _, _| {
            theme::set_source(cr, theme::palette().base.alpha(0.55));
            let _ = cr.paint();
        });
        let stage = gtk::Fixed::new();
        let hints = hint::layer();
        let layers = gtk::Overlay::new();
        layers.set_child(Some(&backdrop));
        layers.add_overlay(&stage);
        layers.add_overlay(&hints);
        window.set_child(Some(&layers));

        let overview = Rc::new(Self {
            window,
            layers,
            stage,
            hints,
            fade: Tween::closed(FADE_MS, MORPH),
            open: Cell::new(false),
            generation: Cell::new(0),
            monitor: RefCell::default(),
            layout: RefCell::default(),
            slots: RefCell::default(),
            columns: Cell::new(1),
            selected: Rc::new(Cell::new(0)),
            drop_hot: Rc::new(Cell::new(None)),
            pictures: Rc::default(),
            tiles: RefCell::default(),
            capture_widths: RefCell::default(),
            wallpaper: Rc::default(),
            wallpaper_path: RefCell::default(),
        });

        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&overview);
        click.connect_released(move |_, _, _, _| weak.upgrade().into_iter().for_each(|o| o.hide()));
        backdrop.add_controller(click);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&overview);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(o) = weak.upgrade() else { return glib::Propagation::Proceed };
            let back = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            let columns = o.columns.get() as isize;
            match key {
                gdk::Key::Escape => o.hide(),
                gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space => o.go_selected(),
                gdk::Key::Left => o.step(-1),
                gdk::Key::Right => o.step(1),
                gdk::Key::Up => o.step(-columns),
                gdk::Key::Down => o.step(columns),
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => o.step(if back || key == gdk::Key::ISO_Left_Tab { -1 } else { 1 }),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        overview.window.add_controller(keys);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(&overview);
        scroll.connect_scroll(move |_, _, dy| {
            if let Some(o) = weak.upgrade().filter(|_| dy != 0.0) {
                o.step(if dy > 0.0 { 1 } else { -1 });
            }
            glib::Propagation::Stop
        });
        overview.window.add_controller(scroll);

        let weak = Rc::downgrade(&overview);
        capture::on_picture(move |address, texture| {
            let Some(o) = weak.upgrade().filter(|o| o.open.get()) else { return };
            let Some(surface) = theme::texture_surface(texture) else { return };
            o.pictures.borrow_mut().insert(address.to_owned(), surface);
            if let Some(tile) = o.tiles.borrow().get(address) {
                tile.icon.set_visible(false);
                tile.area.queue_draw();
            }
        });
        overview
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        if !self.open.get() {
            *self.monitor.borrow_mut() = monitor;
        }
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.open.get() { self.hide() } else { self.show() }
    }

    fn scale(&self) -> f64 {
        config::bar_scale(self.monitor.borrow().as_ref().map_or(1080, |m| m.geometry().height()))
    }

    fn show(self: &Rc<Self>) {
        self.open.set(true);
        self.pictures.borrow_mut().clear();
        self.window.set_monitor(self.monitor.borrow().as_ref());
        let overview = self.clone();
        glib::spawn_future_local(async move {
            if !overview.refresh(true).await {
                overview.open.set(false);
                return;
            }
            overview.layers.set_opacity(0.0);
            hyprland::hold_blur(config::get().launcher_blur);
            overview.window.present();
            overview.animate(1.0);
            overview.load_wallpaper();
            overview.capture_loop();
        });
    }

    fn hide(self: &Rc<Self>) {
        if self.open.replace(false) {
            hyprland::release_blur();
            self.animate(0.0);
        }
    }

    fn animate(self: &Rc<Self>, target: f64) {
        let overview = Rc::downgrade(self);
        anim::animate_to(&self.layers, &self.fade, target, move |layers, t| {
            layers.set_opacity(Easing::OutCubic.apply(t));
            if let Some(o) = overview.upgrade().filter(|o| t <= 0.0 && !o.open.get()) {
                o.window.set_visible(false);
                o.clear();
                o.pictures.borrow_mut().clear();
            }
        });
    }

    /// Asks for every window's picture now and then, while open.
    fn capture_loop(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let overview = Rc::downgrade(self);
        let request = move |o: &Self| {
            let jobs = o.capture_widths.borrow().iter().map(|(address, width)| Job { address: address.clone(), max_width: *width }).collect();
            capture::request(jobs);
        };
        request(self);
        glib::timeout_add_local(FRAME, move || match overview.upgrade() {
            Some(o) if o.open.get() && o.generation.get() == generation => {
                request(&o);
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    fn load_wallpaper(self: &Rc<Self>) {
        let overview = Rc::downgrade(self);
        let height = (f64::from(WALLPAPER_HEIGHT)).round() as i32;
        glib::spawn_future_local(async move {
            let Some(path) = wallpaper::current().await else { return };
            let Some(o) = overview.upgrade() else { return };
            if *o.wallpaper_path.borrow() == path {
                return;
            }
            let decoded = gio::spawn_blocking({
                let path = path.clone();
                move || wallpaper::decode_scaled(&path, height)
            })
            .await;
            let Some((pixels, w, h)) = decoded.ok().flatten() else { return };
            let Ok(surface) = cairo::ImageSurface::create_for_data(pixels, cairo::Format::ARgb32, w, h, w * 4) else { return };
            *o.wallpaper.borrow_mut() = Some(surface);
            *o.wallpaper_path.borrow_mut() = path;
            o.slots.borrow().iter().for_each(|slot| slot.back.queue_draw());
        });
    }

    /// Reads the layout and lays the grid out again; false if Hyprland didn't
    /// answer. `reselect` puts the selection on the active workspace.
    async fn refresh(self: &Rc<Self>, reselect: bool) -> bool {
        let Ok(Ok(layout)) = gio::spawn_blocking(hyprland::layout).await else { return false };
        let connector = self.monitor.borrow().as_ref().and_then(|m| m.connector()).map(String::from);
        let Some(screen) = layout.monitors.iter().find(|m| Some(&m.name) == connector.as_ref()).or(layout.monitors.first()).cloned() else {
            return false;
        };
        let ids: Vec<i32> = layout.workspaces.iter().filter(|w| w.monitor == screen.name).map(|w| w.id).collect();
        if reselect {
            self.selected.set(ids.iter().position(|id| *id == screen.active).unwrap_or(0));
        }
        *self.layout.borrow_mut() = Some((layout, screen));
        self.build();
        true
    }

    fn build(self: &Rc<Self>) {
        let Some((layout, screen)) = self.layout.borrow().clone() else { return };
        let s = self.scale();
        let device = self.monitor.borrow().as_ref().map_or(1, |m| m.scale_factor()).max(1) as f64;
        self.clear();

        let eyebrow = popout::eyebrow(s, "Overview");
        self.stage.put(&eyebrow, 40.0 * s, 32.0 * s);
        // On a pill, so it reads over any wallpaper or window.
        let words = TextStyle::new(10.0 * s, pango::Weight::Medium, SUBTLE)
            .label("Click to go  \u{b7}  drag windows between workspaces  \u{b7}  arrows and Enter  \u{b7}  Esc to close");
        let footer = hint::framed(s, &words, 12.0, 5.0);
        let footer_w = f64::from(footer.measure(gtk::Orientation::Horizontal, -1).1);
        self.stage.put(&footer, ((screen.width - footer_w) / 2.0).round(), (screen.height - 46.0 * s).round());

        let ids: Vec<i32> = layout.workspaces.iter().filter(|w| w.monitor == screen.name).map(|w| w.id).collect();
        let fresh = (1..).find(|id| !layout.workspaces.iter().any(|w| w.id == *id)).unwrap_or(1);
        let count = ids.len().max(1);
        let gap = 26.0 * s;
        let (cell_w, columns) = fit(count, screen.width - 96.0 * s, screen.height - 160.0 * s, screen.width / screen.height, gap, SLAT);
        let cell_h = cell_w * screen.height / screen.width;
        let slat_w = cell_w * SLAT;
        self.columns.set(columns);
        let rows = count.div_ceil(columns);
        let block_h = rows as f64 * cell_h + (rows - 1) as f64 * gap;
        let top = 84.0 * s + ((screen.height - 160.0 * s) - block_h).max(0.0) / 2.0;

        // Workspaces fill the rows; the "+" slat closes the last one.
        let row_width = |row: usize| {
            let cells = if row == rows - 1 { count - row * columns } else { columns };
            let mut width = cells as f64 * cell_w + (cells - 1) as f64 * gap;
            if row == rows - 1 {
                width += gap + slat_w;
            }
            width
        };
        for (i, id) in ids.iter().copied().chain(std::iter::once(fresh)).enumerate() {
            let is_fresh = i == ids.len();
            let (row, column) = if is_fresh { (rows - 1, count - (rows - 1) * columns) } else { (i / columns, i % columns) };
            let x = (screen.width - row_width(row)) / 2.0 + column as f64 * (cell_w + gap);
            let y = top + row as f64 * (cell_h + gap);
            let windows: Vec<Placed> = if is_fresh { Vec::new() } else { layout.windows.iter().filter(|w| w.workspace == id).cloned().collect() };
            let size = (if is_fresh { slat_w } else { cell_w }, cell_h);
            let slot = self.cell(s, i, id, is_fresh, id == screen.active, &windows, &screen, size, device);
            self.stage.put(&slot.widget, x.round(), y.round());
            self.slots.borrow_mut().push(slot);
        }
        self.selected.set(self.selected.get().min(ids.len()));
    }

    /// Empties the stage; the pictures stay for the next layout.
    fn clear(&self) {
        while let Some(child) = self.stage.first_child() {
            self.stage.remove(&child);
        }
        self.slots.borrow_mut().clear();
        self.tiles.borrow_mut().clear();
        self.capture_widths.borrow_mut().clear();
    }

    /// The cell under `at`, a point on the hint layer.
    fn slot_at(&self, at: &graphene::Point) -> Option<usize> {
        self.slots.borrow().iter().position(|slot| slot.widget.compute_bounds(&self.hints).is_some_and(|b| b.contains_point(at)))
    }

    fn set_drop_hot(&self, index: Option<usize>) {
        if self.drop_hot.replace(index) != index {
            self.slots.borrow().iter().for_each(|slot| slot.back.queue_draw());
        }
    }

    // ---- selection -------------------------------------------------------

    fn step(&self, by: isize) {
        let count = self.slots.borrow().len() as isize;
        if count == 0 {
            return;
        }
        let next = (self.selected.get() as isize + by).rem_euclid(count) as usize;
        self.selected.set(next);
        self.slots.borrow().iter().for_each(|slot| slot.back.queue_draw());
    }

    fn go_selected(self: &Rc<Self>) {
        let id = self.slots.borrow().get(self.selected.get()).map(|slot| slot.id);
        if let Some(id) = id {
            self.go(id);
        }
    }

    fn go(self: &Rc<Self>, id: i32) {
        hyprland::focus_workspace(id);
        self.hide();
    }

    // ---- the grid --------------------------------------------------------

    /// A workspace as the wallpaper with its windows on it, its number in a
    /// badge; or the "+" for a fresh one.
    #[allow(clippy::too_many_arguments)]
    fn cell(self: &Rc<Self>, s: f64, index: usize, id: i32, fresh: bool, active: bool, windows: &[Placed], screen: &Screen, (w, h): (f64, f64), device: f64) -> Slot {
        let hovered = Rc::new(Cell::new(false));
        let back = gtk::DrawingArea::builder().width_request(w.round() as i32).height_request(h.round() as i32).build();
        back.set_draw_func({
            let (hovered, selected, drop_hot, wallpaper) = (hovered.clone(), self.selected.clone(), self.drop_hot.clone(), self.wallpaper.clone());
            move |_, cr, w, h| {
                let state = Look { active, picked: selected.get() == index, hovered: hovered.get(), dropping: drop_hot.get() == Some(index) };
                draw_cell(cr, f64::from(w), f64::from(h), s, fresh, state, wallpaper.borrow().as_ref());
            }
        });
        let cell = gtk::Overlay::new();
        cell.set_child(Some(&back));
        cell.set_cursor_from_name(Some("pointer"));

        if !fresh {
            let map = gtk::Fixed::new();
            let (scale_x, scale_y) = (w / screen.width, h / screen.height);
            let mut ordered: Vec<&Placed> = windows.iter().collect();
            ordered.sort_by_key(|p| p.floating);
            for window in ordered {
                let x = ((window.x - screen.x) * scale_x).clamp(0.0, w - 4.0);
                let y = ((window.y - screen.y) * scale_y).clamp(0.0, h - 4.0);
                let tw = (window.width * scale_x).clamp(16.0 * s, w - x);
                let th = (window.height * scale_y).clamp(16.0 * s, h - y);
                let tile = self.window_tile(s, window, (tw, th));
                map.put(&tile, x.round(), y.round());
                self.capture_widths.borrow_mut().insert(window.address.clone(), (tw * device).round() as u32);
            }
            cell.add_overlay(&map);

            // The number, on top of the windows so it always shows.
            let badge = number_badge(s, id, active);
            badge.set_halign(gtk::Align::Start);
            badge.set_valign(gtk::Align::Start);
            badge.set_margin_start((8.0 * s).round() as i32);
            badge.set_margin_top((8.0 * s).round() as i32);
            cell.add_overlay(&badge);
        }

        on_hover(&cell, {
            let back = back.downgrade();
            move |inside| {
                hovered.set(inside);
                back.upgrade().into_iter().for_each(|b| b.queue_draw());
            }
        });
        let click = gtk::GestureClick::new();
        let overview = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| overview.upgrade().into_iter().for_each(|o| o.go(id)));
        cell.add_controller(click);
        Slot { id, widget: cell.upcast(), back }
    }

    /// A window: its live picture, or its icon until one arrives; a close
    /// button on hover; a click focuses it and a drag carries it.
    fn window_tile(self: &Rc<Self>, s: f64, window: &Placed, (w, h): (f64, f64)) -> gtk::Overlay {
        let area = gtk::DrawingArea::builder().width_request(w.round() as i32).height_request(h.round() as i32).build();
        let hovered = Rc::new(Cell::new(false));
        area.set_draw_func({
            let (pictures, address, hovered) = (self.pictures.clone(), window.address.clone(), hovered.clone());
            move |_, cr, w, h| draw_window(cr, f64::from(w), f64::from(h), s, pictures.borrow().get(&address), hovered.get())
        });
        let icon = gtk::Image::from_gicon(&icons::for_class(&window.class));
        icon.set_pixel_size((w.min(h) * 0.4).clamp(14.0 * s, 48.0 * s).round() as i32);
        icon.set_visible(!self.pictures.borrow().contains_key(&window.address));
        icon.set_can_target(false);

        let close = close_button(s);
        close.set_halign(gtk::Align::End);
        close.set_valign(gtk::Align::Start);
        close.set_margin_end((5.0 * s).round() as i32);
        close.set_margin_top((5.0 * s).round() as i32);
        close.set_visible(false);
        let click = gtk::GestureClick::new();
        click.connect_pressed(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        let (overview, address) = (Rc::downgrade(self), window.address.clone());
        click.connect_released(move |_, _, _, _| {
            hyprland::close_window(&address);
            if let Some(o) = overview.upgrade() {
                glib::timeout_add_local_once(SETTLE * 2, move || {
                    glib::spawn_future_local(async move {
                        o.refresh(false).await;
                    });
                });
            }
        });
        close.add_controller(click);

        let tile = gtk::Overlay::new();
        tile.set_child(Some(&area));
        tile.add_overlay(&icon);
        tile.add_overlay(&close);
        tile.set_cursor_from_name(Some("grab"));
        on_hover(&tile, {
            let (area, close) = (area.downgrade(), close.downgrade());
            move |inside| {
                hovered.set(inside);
                area.upgrade().into_iter().for_each(|a| a.queue_draw());
                close.upgrade().into_iter().for_each(|c| c.set_visible(inside));
            }
        });
        let title = if window.title.is_empty() { window.class.clone() } else { window.title.clone() };
        hint::float_below(&tile, &self.hints, s, move || hint::pill(s, &title));
        self.carry(s, &tile, window);
        self.tiles.borrow_mut().insert(window.address.clone(), Tile { area, icon });
        tile
    }

    /// A click on `tile` focuses the window; a drag carries it to a workspace.
    fn carry(self: &Rc<Self>, s: f64, tile: &gtk::Overlay, window: &Placed) {
        let drag = gtk::GestureDrag::new();
        let ghost: Rc<RefCell<Option<gtk::Widget>>> = Rc::default();
        drag.connect_drag_begin(|gesture, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        drag.connect_drag_update({
            let (overview, tile, ghost, address) = (Rc::downgrade(self), tile.downgrade(), ghost.clone(), window.address.clone());
            move |gesture, dx, dy| {
                let (Some(o), Some(tile)) = (overview.upgrade(), tile.upgrade()) else { return };
                let Some((x0, y0)) = gesture.start_point() else { return };
                if ghost.borrow().is_none() {
                    if dx.hypot(dy) < DRAG_START * s {
                        return;
                    }
                    tile.set_opacity(0.3);
                    let carried = carried(s, o.pictures.borrow().get(&address).cloned());
                    o.hints.put(&carried, 0.0, 0.0);
                    *ghost.borrow_mut() = Some(carried.upcast());
                }
                if let (Some(g), Some(at)) = (&*ghost.borrow(), tile.compute_point(&o.hints, &graphene::Point::new((x0 + dx) as f32, (y0 + dy) as f32))) {
                    let (gw, gh) = (f64::from(g.width().max(1)), f64::from(g.height().max(1)));
                    o.hints.move_(g, f64::from(at.x()) - gw / 2.0, f64::from(at.y()) - gh / 2.0);
                    o.set_drop_hot(o.slot_at(&at));
                }
            }
        });
        drag.connect_drag_end({
            let (overview, tile, address, from) = (Rc::downgrade(self), tile.downgrade(), window.address.clone(), window.workspace);
            move |gesture, dx, dy| {
                let (Some(o), Some(tile)) = (overview.upgrade(), tile.upgrade()) else { return };
                let Some(g) = ghost.borrow_mut().take() else {
                    hyprland::focus_window(&address);
                    return o.hide();
                };
                o.hints.remove(&g);
                tile.set_opacity(1.0);
                o.set_drop_hot(None);
                let Some((x0, y0)) = gesture.start_point() else { return };
                let Some(at) = tile.compute_point(&o.hints, &graphene::Point::new((x0 + dx) as f32, (y0 + dy) as f32)) else { return };
                let target = o.slot_at(&at).and_then(|i| o.slots.borrow().get(i).map(|slot| slot.id));
                if let Some(to) = target.filter(|to| *to != from) {
                    hyprland::move_window(&address, to);
                    let o = o.clone();
                    glib::timeout_add_local_once(SETTLE, move || {
                        glib::spawn_future_local(async move {
                            o.refresh(false).await;
                        });
                    });
                }
            }
        });
        tile.add_controller(drag);
    }
}

/// The largest cell width, and the columns for it, that fits `count` cells
/// of the screen's shape into `room_w` by `room_h` with `gap` between, with
/// room left in the last row for the "+" slat, `slat` cells wide. Fewer,
/// longer rows win unless more rows make the cells clearly bigger.
fn fit(count: usize, room_w: f64, room_h: f64, aspect: f64, gap: f64, slat: f64) -> (f64, usize) {
    let count = count.max(1);
    let mut best = (0.0, 1);
    for columns in (1..=count).rev() {
        let rows = count.div_ceil(columns);
        let last = count - (rows - 1) * columns;
        let full_rows = (room_w - gap * (columns - 1) as f64) / columns as f64;
        let last_row = (room_w - gap * last as f64) / (last as f64 + slat);
        let by_height = (room_h - gap * (rows - 1) as f64) / rows as f64 * aspect;
        // No cell bigger than about half the screen, even with one workspace.
        let width = full_rows.min(last_row).min(by_height).min(room_w * 0.46);
        if width > best.0 * 1.1 {
            best = (width, columns);
        }
    }
    best
}

// ---- drawing -----------------------------------------------------------------

/// How a cell is to look.
#[derive(Clone, Copy)]
struct Look {
    active: bool,
    /// Under the keyboard selection.
    picked: bool,
    hovered: bool,
    /// A carried window would land here.
    dropping: bool,
}

fn draw_cell(cr: &cairo::Context, w: f64, h: f64, s: f64, fresh: bool, look: Look, wallpaper: Option<&cairo::ImageSurface>) {
    let Look { active, picked, hovered, dropping } = look;
    let palette = theme::palette();
    let radius = 12.0 * s;
    if fresh {
        theme::rounded_rect(cr, 1.0, 1.0, w - 2.0, h - 2.0, radius);
        theme::set_source(cr, if dropping { palette.accent.alpha(0.28) } else { palette.base.alpha(if hovered { 0.6 } else { 0.4 }) });
        let _ = cr.fill_preserve();
        cr.set_dash(&[6.0 * s, 6.0 * s], 0.0);
        cr.set_line_width(if dropping { 2.5 * s } else { 1.5 * s });
        theme::set_source(cr, if dropping { palette.accent_lit } else if picked { CREAM } else if hovered { palette.edge } else { palette.outline });
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
        let arm = w.min(h) * 0.1;
        cr.move_to(w / 2.0 - arm, h / 2.0);
        cr.line_to(w / 2.0 + arm, h / 2.0);
        cr.move_to(w / 2.0, h / 2.0 - arm);
        cr.line_to(w / 2.0, h / 2.0 + arm);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_width(if dropping { 3.5 * s } else { 2.5 * s });
        theme::set_source(cr, if dropping { palette.accent_lit } else if hovered || picked { CREAM } else { theme::DIM });
        let _ = cr.stroke();
        return;
    }

    let _ = cr.save();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
    cr.clip();
    match wallpaper {
        Some(image) => {
            let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
            let scale = (w / iw).max(h / ih);
            cr.translate((w - iw * scale) / 2.0, (h - ih * scale) / 2.0);
            cr.scale(scale, scale);
            let _ = cr.set_source_surface(image, 0.0, 0.0);
            cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
        }
        None => {
            theme::set_source(cr, palette.recessed);
            let _ = cr.paint();
        }
    }
    let _ = cr.restore();
    // Dim all but the active workspace a touch, so it stands out.
    theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
    theme::set_source(cr, palette.base.alpha(if active || hovered { 0.1 } else { 0.32 }));
    let _ = cr.fill();

    if dropping {
        theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
        theme::set_source(cr, palette.accent.alpha(0.18));
        let _ = cr.fill();
    }
    let (line, color) = match () {
        _ if dropping => (3.0 * s, palette.accent_lit),
        _ if picked => (2.5 * s, CREAM),
        _ if active => (2.0 * s, palette.accent_lit),
        _ if hovered => (1.5 * s, palette.edge),
        _ => (1.0, palette.outline),
    };
    theme::rounded_rect(cr, line / 2.0, line / 2.0, w - line, h - line, radius);
    cr.set_line_width(line);
    theme::set_source(cr, color);
    let _ = cr.stroke();
}

fn draw_window(cr: &cairo::Context, w: f64, h: f64, s: f64, picture: Option<&cairo::ImageSurface>, hovered: bool) {
    let palette = theme::palette();
    let radius = 6.0 * s;
    let inset = 1.5 * s;
    theme::rounded_rect(cr, inset, inset, w - 2.0 * inset, h - 2.0 * inset, radius);
    match picture {
        Some(image) => {
            let _ = cr.save();
            cr.clip_preserve();
            let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
            cr.translate(inset, inset);
            cr.scale((w - 2.0 * inset) / iw, (h - 2.0 * inset) / ih);
            let _ = cr.set_source_surface(image, 0.0, 0.0);
            cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
            let _ = cr.restore();
        }
        None => {
            theme::set_source(cr, palette.raised);
            let _ = cr.fill_preserve();
        }
    }
    cr.set_line_width(if hovered { 1.5 * s } else { 1.0 });
    theme::set_source(cr, if hovered { CREAM } else { BRIGHT.alpha(0.18) });
    let _ = cr.stroke();
}

/// The workspace's number on a small pill, in the accent when it's active.
fn number_badge(s: f64, id: i32, active: bool) -> gtk::Overlay {
    let label = TextStyle::new(12.0 * s, pango::Weight::Bold, if active { theme::palette().base } else { CREAM }).label(&id.to_string());
    let (x, y) = ((9.0 * s).round() as i32, (3.0 * s).round() as i32);
    label.set_margin_start(x);
    label.set_margin_end(x);
    label.set_margin_top(y);
    label.set_margin_bottom(y);
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
        theme::set_source(cr, if active { palette.accent_lit } else { palette.base.alpha(0.82) });
        let _ = cr.fill();
    });
    let badge = gtk::Overlay::builder().can_target(false).build();
    badge.set_child(Some(&back));
    badge.add_overlay(&label);
    badge.set_measure_overlay(&label, true);
    badge
}

/// A round ✕ that reddens on hover.
fn close_button(s: f64) -> gtk::DrawingArea {
    let size = (20.0 * s).round() as i32;
    let button = gtk::DrawingArea::builder().width_request(size).height_request(size).build();
    let hovered = Rc::new(Cell::new(false));
    button.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
            theme::set_source(cr, if hovered.get() { palette.accent_lit } else { palette.base.alpha(0.85) });
            let _ = cr.fill();
            let arm = w * 0.18;
            cr.move_to(w / 2.0 - arm, h / 2.0 - arm);
            cr.line_to(w / 2.0 + arm, h / 2.0 + arm);
            cr.move_to(w / 2.0 + arm, h / 2.0 - arm);
            cr.line_to(w / 2.0 - arm, h / 2.0 + arm);
            cr.set_line_width(1.8 * s);
            cr.set_line_cap(cairo::LineCap::Round);
            theme::set_source(cr, if hovered.get() { palette.base } else { CREAM });
            let _ = cr.stroke();
        }
    });
    button.set_cursor_from_name(Some("pointer"));
    on_hover(&button, {
        let button = button.downgrade();
        move |inside| {
            hovered.set(inside);
            button.upgrade().into_iter().for_each(|b| b.queue_draw());
        }
    });
    button
}

/// What follows the pointer while a window is carried: a small copy of its
/// picture, outlined in the accent.
fn carried(s: f64, picture: Option<cairo::ImageSurface>) -> gtk::DrawingArea {
    let (w, h) = match &picture {
        Some(p) => {
            let w = 120.0 * s;
            (w, w * f64::from(p.height()) / f64::from(p.width().max(1)))
        }
        None => (48.0 * s, 48.0 * s),
    };
    let ghost = gtk::DrawingArea::builder().width_request(w.round() as i32).height_request(h.round() as i32).can_target(false).build();
    ghost.set_draw_func(move |_, cr, w, h| {
        draw_window(cr, f64::from(w), f64::from(h), s, picture.as_ref(), false);
        theme::rounded_rect(cr, 1.0, 1.0, f64::from(w) - 2.0, f64::from(h) - 2.0, 6.0 * s);
        cr.set_line_width(2.0 * s);
        theme::set_source(cr, theme::palette().accent_lit);
        let _ = cr.stroke();
    });
    ghost
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fits_cells_and_the_slat_in_the_room() {
        let (room_w, room_h, aspect, gap, slat) = (1800.0, 900.0, 1.6, 20.0, 0.3);
        for count in 1..=12 {
            let (width, columns) = fit(count, room_w, room_h, aspect, gap, slat);
            let rows = count.div_ceil(columns);
            let last = count - (rows - 1) * columns;
            assert!(columns as f64 * width + (columns - 1) as f64 * gap <= room_w + 0.01, "{count} cells overflow the width");
            assert!(last as f64 * width + last as f64 * gap + slat * width <= room_w + 0.01, "{count} cells leave no room for the slat");
            assert!(rows as f64 * width / aspect + (rows - 1) as f64 * gap <= room_h + 0.01, "{count} cells overflow the height");
        }
        // Two workspaces sit side by side with the slat, even when two rows
        // would make them a touch bigger (as on this 16:10 screen).
        assert_eq!(fit(2, room_w, room_h, aspect, gap, slat).1, 2);
        assert_eq!(fit(2, 1782.0, 970.0, 1.6, 37.0, slat).1, 2);
    }
}
