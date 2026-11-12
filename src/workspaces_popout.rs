//! Workspaces popout, dropping from the workspace pills on a right click: a
//! row of tiles, one per workspace on this screen, each a small map with its
//! windows drawn as app cards where they sit, and a "+" tile for a fresh
//! workspace. Click a tile to switch to it, click a window to focus it, drag
//! a window onto a tile to move it there.
//!
//! Hyprland doesn't draw workspaces that aren't showing, so these are maps,
//! not pictures. The layout is read when the popout opens and after a move.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene, pango};
use gtk4 as gtk;

use crate::hint;
use crate::hyprland::{self, Layout, Placed, Screen};
use crate::icons;
use crate::popout::{self, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-workspaces";
const TILE_WIDTH: f64 = 170.0;
/// How far a press must travel before it's a drag rather than a click.
const DRAG_START: f64 = 6.0;
/// Hyprland takes a moment to settle a move before the layout is read again.
const SETTLE: Duration = Duration::from_millis(150);

pub struct WorkspacesPopout {
    popout: Rc<Popout>,
    /// Where windows can be dropped: each tile and its workspace.
    targets: RefCell<Vec<(i32, gtk::Widget)>>,
}

impl WorkspacesPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self { popout: Popout::new(app, NAMESPACE), targets: RefCell::default() })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let popout = self.clone();
        glib::spawn_future_local(async move {
            if popout.refresh().await {
                popout.popout.show(Anchor::Under(x));
            }
        });
    }

    /// Reads the layout and redraws the strip; false if Hyprland didn't answer.
    async fn refresh(self: &Rc<Self>) -> bool {
        let Ok(Ok(layout)) = gio::spawn_blocking(hyprland::layout).await else { return false };
        let connector = self.popout.connector();
        let Some(screen) = layout.monitors.iter().find(|m| Some(&m.name) == connector.as_ref()).or(layout.monitors.first()).cloned() else {
            return false;
        };
        let s = self.popout.scale();
        self.popout.set_content(&popout::card(&self.body(s, &layout, &screen)));
        true
    }

    fn close_after(self: &Rc<Self>, act: impl FnOnce()) {
        act();
        self.popout.hide();
    }

    fn body(self: &Rc<Self>, s: f64, layout: &Layout, screen: &Screen) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(12.0))
            .margin_top(px(13.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();

        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        head.append(&theme::filled_icon_glyph("space_dashboard", 16.0 * s, theme::palette().accent));
        let title = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("WORKSPACES");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        head.append(&title);
        head.append(&TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).label("Drag a window to move it"));
        column.append(&head);

        let ids: Vec<i32> = layout.workspaces.iter().filter(|w| w.monitor == screen.name).map(|w| w.id).collect();
        let fresh = (1..).find(|id| !layout.workspaces.iter().any(|w| w.id == *id)).unwrap_or(1);
        // Tiles share the screen's shape and shrink to keep the row on screen.
        let gap = 10.0 * s;
        let count = ids.len() as f64 + 1.0;
        let room = screen.width * 0.85 - 32.0 * s;
        let width = (TILE_WIDTH * s).min((room - gap * (count - 1.0)) / count).max(60.0 * s);
        let height = width * screen.height / screen.width;

        self.targets.borrow_mut().clear();
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, gap.round() as i32);
        for id in ids {
            let windows: Vec<&Placed> = layout.windows.iter().filter(|w| w.workspace == id).collect();
            strip.append(&self.tile(s, id, id == screen.active, &windows, screen, (width, height)));
        }
        strip.append(&self.fresh_tile(s, fresh, (width, height)));
        column.append(&strip);
        column
    }

    /// A workspace: its map, and its number beneath.
    fn tile(self: &Rc<Self>, s: f64, id: i32, active: bool, windows: &[&Placed], screen: &Screen, (w, h): (f64, f64)) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let hovered = Rc::new(Cell::new(false));
        let back = gtk::DrawingArea::builder().width_request(w.round() as i32).height_request(h.round() as i32).build();
        back.set_draw_func({
            let hovered = hovered.clone();
            move |_, cr, w, h| {
                let palette = theme::palette();
                let (w, h) = (f64::from(w), f64::from(h));
                theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 8.0 * s);
                theme::set_source(cr, palette.recessed);
                let _ = cr.fill_preserve();
                cr.set_line_width(if active { 1.5 * s } else { 1.0 });
                theme::set_source(cr, if active { palette.accent_lit } else if hovered.get() { palette.edge } else { palette.outline });
                let _ = cr.stroke();
            }
        });
        let map = gtk::Fixed::new();
        // Tiled windows first, so floating ones sit on top as they do on screen.
        let (scale_x, scale_y) = (w / screen.width, h / screen.height);
        let mut ordered: Vec<&&Placed> = windows.iter().collect();
        ordered.sort_by_key(|p| p.floating);
        for window in ordered {
            let x = ((window.x - screen.x) * scale_x).clamp(0.0, w - 4.0);
            let y = ((window.y - screen.y) * scale_y).clamp(0.0, h - 4.0);
            let cw = (window.width * scale_x).clamp(12.0 * s, w - x);
            let ch = (window.height * scale_y).clamp(12.0 * s, h - y);
            let card = self.window_card(s, window, (cw, ch));
            map.put(&card, x.round(), y.round());
        }
        let tile = gtk::Overlay::new();
        tile.set_child(Some(&back));
        tile.add_overlay(&map);
        tile.set_cursor_from_name(Some("pointer"));
        on_hover(&tile, move |inside| {
            hovered.set(inside);
            back.queue_draw();
        });
        let click = gtk::GestureClick::new();
        let popout = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| {
            if let Some(p) = popout.upgrade() {
                p.close_after(|| hyprland::focus_workspace(id));
            }
        });
        tile.add_controller(click);
        self.targets.borrow_mut().push((id, tile.clone().upcast()));

        let column = gtk::Box::new(gtk::Orientation::Vertical, px(5.0));
        column.append(&tile);
        let number = TextStyle::new(10.0 * s, if active { pango::Weight::Semibold } else { pango::Weight::Medium }, if active { CREAM } else { DIM }).label(&id.to_string());
        column.append(&number);
        column
    }

    /// The "+" tile: click for a fresh workspace, or drop a window on it.
    fn fresh_tile(self: &Rc<Self>, s: f64, id: i32, (w, h): (f64, f64)) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let hovered = Rc::new(Cell::new(false));
        let tile = gtk::DrawingArea::builder().width_request(w.round() as i32).height_request(h.round() as i32).build();
        tile.set_draw_func({
            let hovered = hovered.clone();
            move |_, cr, w, h| {
                let palette = theme::palette();
                let (w, h) = (f64::from(w), f64::from(h));
                theme::rounded_rect(cr, 0.75, 0.75, w - 1.5, h - 1.5, 8.0 * s);
                cr.set_dash(&[4.0 * s, 4.0 * s], 0.0);
                cr.set_line_width(1.5 * s);
                theme::set_source(cr, if hovered.get() { palette.edge } else { palette.outline });
                let _ = cr.stroke();
                cr.set_dash(&[], 0.0);
                let arm = h.min(w) * 0.14;
                cr.move_to(w / 2.0 - arm, h / 2.0);
                cr.line_to(w / 2.0 + arm, h / 2.0);
                cr.move_to(w / 2.0, h / 2.0 - arm);
                cr.line_to(w / 2.0, h / 2.0 + arm);
                cr.set_line_cap(gtk::cairo::LineCap::Round);
                theme::set_source(cr, if hovered.get() { CREAM } else { DIM });
                let _ = cr.stroke();
            }
        });
        tile.set_cursor_from_name(Some("pointer"));
        on_hover(&tile, {
            let tile = tile.downgrade();
            move |inside| {
                hovered.set(inside);
                tile.upgrade().into_iter().for_each(|t| t.queue_draw());
            }
        });
        let click = gtk::GestureClick::new();
        let popout = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| {
            if let Some(p) = popout.upgrade() {
                p.close_after(|| hyprland::focus_workspace(id));
            }
        });
        tile.add_controller(click);
        self.targets.borrow_mut().push((id, tile.clone().upcast()));

        let column = gtk::Box::new(gtk::Orientation::Vertical, px(5.0));
        column.append(&tile);
        column.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, FAINT).label("New"));
        column
    }

    /// A window on a map: its app's icon on a card its scaled size. Its
    /// title shows on hover; a click focuses it, a drag carries it.
    fn window_card(self: &Rc<Self>, s: f64, window: &Placed, (w, h): (f64, f64)) -> gtk::Overlay {
        let back = gtk::DrawingArea::builder().width_request(w.round() as i32).height_request(h.round() as i32).build();
        back.set_draw_func(move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 1.0, 1.0, w - 2.0, h - 2.0, 5.0 * s);
            theme::set_source(cr, palette.raised);
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, palette.edge.alpha(0.6));
            let _ = cr.stroke();
        });
        let icon = gtk::Image::from_gicon(&icons::for_class(&window.class));
        icon.set_pixel_size((w.min(h) * 0.5).clamp(10.0 * s, 30.0 * s).round() as i32);
        let card = gtk::Overlay::new();
        card.set_child(Some(&back));
        card.add_overlay(&icon);
        card.set_cursor_from_name(Some("grab"));

        let title = if window.title.is_empty() { window.class.clone() } else { window.title.clone() };
        hint::float_below(&card, self.popout.hints(), s, move || hint::pill(s, &title));

        let drag = gtk::GestureDrag::new();
        let dragging: Rc<RefCell<Option<gtk::Overlay>>> = Rc::default();
        drag.connect_drag_begin(|gesture, _, _| {
            // The card's own; the tile under it shouldn't take this as a click.
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        drag.connect_drag_update({
            let (popout, card, dragging, class) = (Rc::downgrade(self), card.downgrade(), dragging.clone(), window.class.clone());
            move |gesture, dx, dy| {
                let (Some(p), Some(card)) = (popout.upgrade(), card.upgrade()) else { return };
                let Some((x0, y0)) = gesture.start_point() else { return };
                if dragging.borrow().is_none() {
                    if dx.hypot(dy) < DRAG_START * s {
                        return;
                    }
                    card.set_opacity(0.35);
                    let ghost = ghost(s, &class);
                    p.popout.hints().put(&ghost, 0.0, 0.0);
                    *dragging.borrow_mut() = Some(ghost);
                }
                if let (Some(ghost), Some(at)) = (&*dragging.borrow(), card.compute_point(p.popout.hints(), &graphene::Point::new((x0 + dx) as f32, (y0 + dy) as f32))) {
                    let half = 18.0 * s;
                    p.popout.hints().move_(ghost, f64::from(at.x()) - half, f64::from(at.y()) - half);
                }
            }
        });
        drag.connect_drag_end({
            let (popout, card, address, from) = (Rc::downgrade(self), card.downgrade(), window.address.clone(), window.workspace);
            move |gesture, dx, dy| {
                let (Some(p), Some(card)) = (popout.upgrade(), card.upgrade()) else { return };
                let Some(ghost) = dragging.borrow_mut().take() else {
                    // Never went far enough: a click.
                    let address = address.clone();
                    return p.close_after(move || hyprland::focus_window(&address));
                };
                p.popout.hints().remove(&ghost);
                card.set_opacity(1.0);
                let Some((x0, y0)) = gesture.start_point() else { return };
                let Some(at) = card.compute_point(p.popout.hints(), &graphene::Point::new((x0 + dx) as f32, (y0 + dy) as f32)) else { return };
                let target = p.targets.borrow().iter().find(|(_, tile)| tile.compute_bounds(p.popout.hints()).is_some_and(|b| b.contains_point(&at))).map(|(id, _)| *id);
                if let Some(to) = target.filter(|to| *to != from) {
                    hyprland::move_window(&address, to);
                    let p = p.clone();
                    glib::timeout_add_local_once(SETTLE, move || {
                        glib::spawn_future_local(async move {
                            p.refresh().await;
                        });
                    });
                }
            }
        });
        card.add_controller(drag);
        card
    }
}

/// What follows the pointer while a window is carried: its icon on a card.
fn ghost(s: f64, class: &str) -> gtk::Overlay {
    let size = (36.0 * s).round() as i32;
    let back = gtk::DrawingArea::builder().width_request(size).height_request(size).build();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 8.0 * s);
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.5 * s);
        theme::set_source(cr, palette.accent_lit);
        let _ = cr.stroke();
    });
    let icon = gtk::Image::from_gicon(&icons::for_class(class));
    icon.set_pixel_size((22.0 * s).round() as i32);
    let ghost = gtk::Overlay::builder().can_target(false).build();
    ghost.set_child(Some(&back));
    ghost.add_overlay(&icon);
    ghost
}
