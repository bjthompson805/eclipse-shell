//! The wallpaper switcher: a card over the blurred desktop with a large
//! preview of the pick, a colour filter, and a strip of tiles below.
//!
//! As in the launcher, hover and the keyboard selection are separate: the
//! preview follows the hovered tile, else the selection, which the arrows and
//! Tab move. A click or Enter sets the wallpaper and closes; Escape or a
//! click off the card just closes.
//!
//! Thumbnails and their colour readings are made on a worker thread and kept
//! for the session (redone only when a file changes); previews decode at the
//! preview's size, also off the main thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::anim::{self, Easing, Tween, MORPH};
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, DIM, FAINT, HAIR, SCRIM};
use crate::{config, hyprland, popout, wallpaper};

const NAMESPACE: &str = "eclipse-shell-wallpapers";
const OPEN_MS: f64 = 240.0;
const CROSSFADE_MS: f64 = 180.0;
/// Card content width before scaling; the preview takes the screen's shape.
const WIDTH: f64 = 660.0;
const TILE_W: f64 = 132.0;
const TILE_H: f64 = 82.0;
/// Thumbnail height in device pixels.
const THUMB_H: i32 = 180;
/// Hue groups for the filter: (name, centre hue). Grey images go in "Mono".
const GROUPS: &[(&str, f64)] = &[("Red", 0.0), ("Orange", 28.0), ("Yellow", 55.0), ("Green", 120.0), ("Cyan", 180.0), ("Blue", 222.0), ("Purple", 275.0), ("Pink", 325.0)];
const MONO: usize = GROUPS.len();

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    size: Option<(i32, i32)>,
    thumb: Option<cairo::ImageSurface>,
    /// Index into GROUPS, or MONO.
    group: usize,
}

/// A thumbnail and its group, made on the worker, keyed by path and mtime.
type Made = (PathBuf, Option<SystemTime>, Option<(Vec<u8>, i32, i32)>, usize, Option<(i32, i32)>);

pub struct WallpaperSwitcher {
    window: gtk::ApplicationWindow,
    layers: gtk::Overlay,
    card_slot: gtk::Box,
    fade: Rc<RefCell<Tween>>,
    open: Cell<bool>,
    monitor: RefCell<Option<gdk::Monitor>>,
    /// Session cache: path -> (mtime, entry).
    cache: RefCell<HashMap<PathBuf, (Option<SystemTime>, Entry)>>,
    entries: RefCell<Vec<Entry>>,
    filter: Cell<Option<usize>>,
    /// Index into the filtered list.
    selected: Cell<usize>,
    hovered: Cell<Option<usize>>,
    current: RefCell<Option<PathBuf>>,
    /// Preview decodes at the preview's size, by path.
    previews: RefCell<HashMap<PathBuf, cairo::ImageSurface>>,
    /// What the preview shows, what it showed, and the crossfade between.
    shown: RefCell<Option<PathBuf>>,
    last_shown: RefCell<Option<PathBuf>>,
    crossfade: Rc<RefCell<Tween>>,
    view: RefCell<Option<View>>,
}

/// The open card's live parts.
struct View {
    preview: gtk::DrawingArea,
    name: gtk::Label,
    detail: gtk::Label,
    live: gtk::Overlay,
    strip: gtk::Box,
    scroller: gtk::ScrolledWindow,
    filters: gtk::Box,
    count: gtk::Label,
}

impl WallpaperSwitcher {
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

        let scrim = gtk::DrawingArea::new();
        scrim.set_draw_func(|_, cr, _, _| {
            theme::set_source(cr, SCRIM);
            let _ = cr.paint();
        });
        let card_slot = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        let layers = gtk::Overlay::new();
        layers.set_child(Some(&scrim));
        layers.add_overlay(&card_slot);
        window.set_child(Some(&layers));

        let switcher = Rc::new(Self {
            window,
            layers,
            card_slot,
            fade: Tween::closed(OPEN_MS, MORPH),
            open: Cell::new(false),
            monitor: RefCell::default(),
            cache: RefCell::default(),
            entries: RefCell::default(),
            filter: Cell::new(None),
            selected: Cell::new(0),
            hovered: Cell::new(None),
            current: RefCell::default(),
            previews: RefCell::default(),
            shown: RefCell::default(),
            last_shown: RefCell::default(),
            crossfade: Tween::shared(CROSSFADE_MS, Easing::OutCubic),
            view: RefCell::default(),
        });

        let click = gtk::GestureClick::new();
        click.connect_released({
            let switcher = Rc::downgrade(&switcher);
            move |_, _, _, _| switcher.upgrade().into_iter().for_each(|s| s.hide())
        });
        scrim.add_controller(click);

        // Before the strip's scroller can take the arrows.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let switcher = Rc::downgrade(&switcher);
            move |_, key, _, _| {
                let Some(s) = switcher.upgrade() else { return glib::Propagation::Proceed };
                match key {
                    gdk::Key::Escape => s.hide(),
                    gdk::Key::Left | gdk::Key::Up => s.step(-1),
                    gdk::Key::Right | gdk::Key::Down => s.step(1),
                    gdk::Key::Tab => s.step(1),
                    gdk::Key::ISO_Left_Tab => s.step(-1),
                    gdk::Key::Home => s.select(0),
                    gdk::Key::End => s.select(usize::MAX),
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        if let Some(e) = s.filtered().get(s.selected.get()).cloned() {
                            s.apply(&e.path);
                        }
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        });
        switcher.window.add_controller(keys);
        switcher
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

    fn device_scale(&self) -> f64 {
        f64::from(self.monitor.borrow().as_ref().map_or(1, |m| m.scale_factor()).max(1))
    }

    /// The preview's size in logical px, shaped like the screen.
    fn preview_size(&self) -> (f64, f64) {
        let s = self.scale();
        let (sw, sh) = self.monitor.borrow().as_ref().map_or((16, 10), |m| (m.geometry().width(), m.geometry().height()));
        let w = WIDTH * s;
        (w, (w * f64::from(sh) / f64::from(sw)).round())
    }

    fn show(self: &Rc<Self>) {
        self.open.set(true);
        self.hovered.set(None);
        self.filter.set(None);
        self.window.set_monitor(self.monitor.borrow().as_ref());
        let switcher = self.clone();
        glib::spawn_future_local(async move {
            // What was set (a clip, not its poster), else what awww shows.
            let current = match wallpaper::recorded() {
                Some(p) => Some(p),
                None => wallpaper::current().await.map(PathBuf::from),
            };
            switcher.load_entries().await;
            if !switcher.open.get() {
                return;
            }
            *switcher.current.borrow_mut() = current.clone();
            // Start on what's showing.
            let at = current.and_then(|c| switcher.entries.borrow().iter().position(|e| e.path == c)).unwrap_or(0);
            switcher.selected.set(at);
            switcher.build();
            switcher.layers.set_opacity(0.0);
            hyprland::hold_blur(config::get().launcher_blur);
            switcher.window.present();
            switcher.animate(1.0);
            switcher.show_preview(true);
            switcher.scroll_to_selected();
        });
    }

    fn hide(self: &Rc<Self>) {
        if self.open.replace(false) {
            hyprland::release_blur();
            self.animate(0.0);
        }
    }

    fn animate(self: &Rc<Self>, target: f64) {
        let switcher = Rc::downgrade(self);
        anim::animate_to(&self.layers, &self.fade, target, move |layers, t| {
            let eased = Easing::OutCubic.apply(t);
            layers.set_opacity(eased);
            if let Some(s) = switcher.upgrade() {
                s.card_slot.set_margin_top(((1.0 - eased) * 16.0 * s.scale()).round() as i32);
                if t <= 0.0 && !s.open.get() {
                    s.window.set_visible(false);
                    GtkWindowExt::set_focus(&s.window, None::<&gtk::Widget>);
                    *s.view.borrow_mut() = None;
                    s.previews.borrow_mut().clear();
                    while let Some(child) = s.card_slot.first_child() {
                        s.card_slot.remove(&child);
                    }
                }
            }
        });
    }

    /// Lists the folder and makes any missing or stale thumbnails.
    async fn load_entries(&self) {
        let paths = wallpaper::list();
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let stale: Vec<PathBuf> = paths.iter().filter(|p| self.cache.borrow().get(*p).is_none_or(|(t, _)| *t != mtime(p))).cloned().collect();
        if !stale.is_empty() {
            let height = (f64::from(THUMB_H) * self.device_scale() / 2.0).max(f64::from(THUMB_H)) as i32;
            let made: Vec<Made> = gio::spawn_blocking(move || {
                stale
                    .into_iter()
                    .map(|p| {
                        let thumb = wallpaper::still(&p).and_then(|still| still.to_str().and_then(|s| wallpaper::decode_scaled(s, height)));
                        let group = thumb.as_ref().and_then(|(d, _, _)| wallpaper::hue(d)).map_or(MONO, nearest_group);
                        let size = wallpaper::size(&p);
                        let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                        (p, t, thumb, group, size)
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();
            let mut cache = self.cache.borrow_mut();
            for (path, t, thumb, group, size) in made {
                let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let thumb = thumb.and_then(|(d, w, h)| surface(d, w, h));
                cache.insert(path.clone(), (t, Entry { path, name, size, thumb, group }));
            }
        }
        let cache = self.cache.borrow();
        *self.entries.borrow_mut() = paths.iter().filter_map(|p| cache.get(p).map(|(_, e)| e.clone())).collect();
    }

    fn filtered(&self) -> Vec<Entry> {
        let filter = self.filter.get();
        self.entries.borrow().iter().filter(|e| filter.is_none_or(|g| e.group == g)).cloned().collect()
    }

    fn step(self: &Rc<Self>, delta: i32) {
        let count = self.filtered().len();
        if count == 0 {
            return;
        }
        let at = (self.selected.get() as i64 + i64::from(delta)).rem_euclid(count as i64) as usize;
        self.select(at);
    }

    fn select(self: &Rc<Self>, index: usize) {
        let count = self.filtered().len();
        if count == 0 {
            return;
        }
        self.selected.set(index.min(count - 1));
        self.redraw_strip();
        self.show_preview(false);
        self.scroll_to_selected();
    }

    fn apply(self: &Rc<Self>, path: &Path) {
        wallpaper::set(path);
        self.hide();
    }

    // ── The card ────────────────────────────────────────────────────────────

    fn build(self: &Rc<Self>) {
        let s = self.scale();
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::new(gtk::Orientation::Vertical, px(12.0));
        column.set_margin_start(px(18.0));
        column.set_margin_end(px(18.0));
        column.set_margin_top(px(16.0));
        column.set_margin_bottom(px(14.0));

        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        let eyebrow = popout::eyebrow(s, "Wallpaper");
        eyebrow.set_hexpand(true);
        head.append(&eyebrow);
        let count = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().tracking(1.0 * s).label("");
        head.append(&count);
        column.append(&head);

        // The preview, with the name and size over a scrim at its foot.
        let (pw, ph) = self.preview_size();
        let preview = gtk::DrawingArea::builder().content_width(pw as i32).content_height(ph as i32).build();
        preview.set_draw_func({
            let switcher = Rc::downgrade(self);
            move |_, cr, w, h| {
                if let Some(sw) = switcher.upgrade() {
                    sw.draw_preview(cr, f64::from(w), f64::from(h), s);
                }
            }
        });
        let caption = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        caption.set_valign(gtk::Align::End);
        caption.set_margin_start(px(16.0));
        caption.set_margin_end(px(16.0));
        caption.set_margin_bottom(px(12.0));
        let names = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
        names.set_hexpand(true);
        let name = TextStyle::new(14.0 * s, pango::Weight::Semibold, BRIGHT).label("");
        name.set_xalign(0.0);
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_max_width_chars(1);
        name.set_hexpand(true);
        let detail = TextStyle::new(9.5 * s, pango::Weight::Medium, CREAM.alpha(0.75)).mono().tracking(0.8 * s).label("");
        detail.set_xalign(0.0);
        names.append(&name);
        names.append(&detail);
        caption.append(&names);
        let live = on_screen_tag(s);
        live.set_valign(gtk::Align::End);
        caption.append(&live);
        let frame = gtk::Overlay::new();
        frame.set_child(Some(&preview));
        frame.add_overlay(&caption);
        column.append(&frame);

        let filters = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        column.append(&filters);

        let strip = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        strip.set_margin_top(px(4.0));
        strip.set_margin_bottom(px(8.0));
        let scroller = gtk::ScrolledWindow::builder().vscrollbar_policy(gtk::PolicyType::Never).hscrollbar_policy(gtk::PolicyType::Automatic).child(&strip).width_request(pw as i32).build();
        column.append(&scroller);

        let foot = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        let folder = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label(&wallpaper::dir().display().to_string().replace(&glib::home_dir().display().to_string(), "~"));
        folder.set_hexpand(true);
        folder.set_xalign(0.0);
        foot.append(&folder);
        foot.append(&TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label("←→ choose · ↵ set · esc close"));
        column.append(&foot);

        let card = popout::card(&column);
        // Clicks on the card stay on the card.
        card.add_controller(gtk::GestureClick::new());
        // Keys all go to the window's handler. Focus left on a widget of a
        // card that's since gone would keep key presses from reaching it.
        card.set_can_focus(false);
        GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        while let Some(child) = self.card_slot.first_child() {
            self.card_slot.remove(&child);
        }
        self.card_slot.append(&card);
        *self.view.borrow_mut() = Some(View { preview, name, detail, live, strip, scroller, filters, count });
        self.fill_filters();
        self.fill_strip();
    }

    fn fill_filters(self: &Rc<Self>) {
        let Some((filters, count)) = self.view.borrow().as_ref().map(|v| (v.filters.clone(), v.count.clone())) else { return };
        let s = self.scale();
        while let Some(child) = filters.first_child() {
            filters.remove(&child);
        }
        let entries = self.entries.borrow().clone();
        let shown = self.filtered().len();
        count.set_text(&if shown == entries.len() { format!("{} WALLPAPERS", entries.len()) } else { format!("{shown} OF {}", entries.len()) });
        let chosen = self.filter.get();
        let all = TextStyle::new(9.5 * s, pango::Weight::Bold, if chosen.is_none() { theme::palette().accent_lit } else { DIM }).mono().tracking(1.4 * s).label("ALL");
        all.set_margin_end((4.0 * s).round() as i32);
        let switcher = Rc::downgrade(self);
        popout::on_click(&all, move || {
            if let Some(sw) = switcher.upgrade() {
                sw.set_filter(None);
            }
        });
        filters.append(&all);
        // Only the groups some wallpaper falls in.
        let mut groups: Vec<usize> = entries.iter().map(|e| e.group).collect();
        groups.sort_unstable();
        groups.dedup();
        for group in groups {
            let n = entries.iter().filter(|e| e.group == group).count();
            let swatch = swatch(s, group, chosen == Some(group));
            let switcher = Rc::downgrade(self);
            popout::on_click(&swatch, move || {
                if let Some(sw) = switcher.upgrade() {
                    sw.set_filter(if sw.filter.get() == Some(group) { None } else { Some(group) });
                }
            });
            let name = if group == MONO { "Mono" } else { GROUPS[group].0 };
            let label = TextStyle::new(9.0 * s, pango::Weight::Medium, if chosen == Some(group) { CREAM } else { FAINT }).label(&format!("{name} {n}"));
            let pair = gtk::Box::new(gtk::Orientation::Horizontal, (5.0 * s).round() as i32);
            pair.append(&swatch);
            pair.append(&label);
            pair.set_margin_start((6.0 * s).round() as i32);
            let switcher = Rc::downgrade(self);
            popout::on_click(&label, move || {
                if let Some(sw) = switcher.upgrade() {
                    sw.set_filter(if sw.filter.get() == Some(group) { None } else { Some(group) });
                }
            });
            filters.append(&pair);
        }
    }

    fn set_filter(self: &Rc<Self>, filter: Option<usize>) {
        let keep = self.filtered().get(self.selected.get()).map(|e| e.path.clone());
        self.filter.set(filter);
        // Stay on the same wallpaper if it's still listed.
        let at = keep.and_then(|p| self.filtered().iter().position(|e| e.path == p)).unwrap_or(0);
        self.selected.set(at);
        self.hovered.set(None);
        self.fill_filters();
        self.fill_strip();
        self.show_preview(false);
        self.scroll_to_selected();
    }

    fn fill_strip(self: &Rc<Self>) {
        let Some(strip) = self.view.borrow().as_ref().map(|v| v.strip.clone()) else { return };
        let s = self.scale();
        while let Some(child) = strip.first_child() {
            strip.remove(&child);
        }
        for (i, entry) in self.filtered().into_iter().enumerate() {
            let tile = gtk::DrawingArea::builder().content_width((TILE_W * s).round() as i32).content_height((TILE_H * s).round() as i32).build();
            tile.set_draw_func({
                let switcher = Rc::downgrade(self);
                move |_, cr, w, h| {
                    if let Some(sw) = switcher.upgrade() {
                        let current = sw.current.borrow().as_ref() == Some(&entry.path);
                        draw_tile(cr, f64::from(w), f64::from(h), s, &entry, i == sw.selected.get(), sw.hovered.get() == Some(i), current);
                    }
                }
            });
            let switcher = Rc::downgrade(self);
            popout::on_hover(&tile, move |inside| {
                let Some(sw) = switcher.upgrade() else { return };
                if inside {
                    sw.hovered.set(Some(i));
                } else if sw.hovered.get() == Some(i) {
                    sw.hovered.set(None);
                }
                sw.redraw_strip();
                sw.show_preview(false);
            });
            let switcher = Rc::downgrade(self);
            popout::on_click(&tile, move || {
                if let Some(sw) = switcher.upgrade() {
                    if let Some(e) = sw.filtered().get(i).cloned() {
                        sw.apply(&e.path);
                    }
                }
            });
            strip.append(&tile);
        }
    }

    fn redraw_strip(&self) {
        if let Some(view) = self.view.borrow().as_ref() {
            let mut child = view.strip.first_child();
            while let Some(c) = child {
                c.queue_draw();
                child = c.next_sibling();
            }
        }
    }

    fn scroll_to_selected(&self) {
        let Some(view) = self.view.borrow().as_ref().map(|v| (v.scroller.clone(), v.strip.clone())) else { return };
        let (scroller, strip) = view;
        let index = self.selected.get();
        glib::idle_add_local_once(move || {
            let mut child = strip.first_child();
            for _ in 0..index {
                child = child.and_then(|c| c.next_sibling());
            }
            let Some(tile) = child else { return };
            let Some(bounds) = tile.compute_bounds(&strip) else { return };
            let adj = scroller.hadjustment();
            let (x, w) = (f64::from(bounds.x()), f64::from(bounds.width()));
            if x < adj.value() {
                adj.set_value(x);
            } else if x + w > adj.value() + adj.page_size() {
                adj.set_value(x + w - adj.page_size());
            }
        });
    }

    // ── The preview ─────────────────────────────────────────────────────────

    /// Shows the hovered wallpaper, else the selected one; `instant` skips the crossfade.
    fn show_preview(self: &Rc<Self>, instant: bool) {
        let filtered = self.filtered();
        let Some(entry) = self.hovered.get().or(Some(self.selected.get())).and_then(|i| filtered.get(i)).cloned() else { return };
        if let Some(view) = self.view.borrow().as_ref() {
            view.name.set_text(&entry.name);
            let mut ext = entry.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            if wallpaper::is_video(&entry.path) {
                ext = format!("LIVE {ext}");
            }
            let group = if entry.group == MONO { "MONO" } else { GROUPS[entry.group].0 };
            view.detail.set_text(&match entry.size {
                Some((w, h)) => format!("{w} × {h} · {ext} · {}", group.to_uppercase()),
                None => format!("{ext} · {}", group.to_uppercase()),
            });
            view.live.set_visible(self.current.borrow().as_ref() == Some(&entry.path));
        }
        if self.shown.borrow().as_ref() == Some(&entry.path) {
            return;
        }
        *self.last_shown.borrow_mut() = self.shown.replace(Some(entry.path.clone()));
        let Some(preview) = self.view.borrow().as_ref().map(|v| v.preview.clone()) else { return };
        if instant {
            self.crossfade.borrow_mut().jump(1.0);
            preview.queue_draw();
        } else {
            self.crossfade.borrow_mut().jump(0.0);
            anim::animate_to(&preview, &self.crossfade, 1.0, |p, _| p.queue_draw());
        }
        if !self.previews.borrow().contains_key(&entry.path) {
            let height = (self.preview_size().1 * self.device_scale()).round() as i32;
            let path = entry.path.clone();
            let switcher = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let p = path.clone();
                let decoded = gio::spawn_blocking(move || wallpaper::still(&p).and_then(|still| still.to_str().and_then(|s| wallpaper::decode_scaled(s, height)))).await.ok().flatten();
                let (Some(sw), Some((d, w, h))) = (switcher.upgrade(), decoded) else { return };
                if !sw.open.get() {
                    return;
                }
                if let Some(surface) = surface(d, w, h) {
                    sw.previews.borrow_mut().insert(path, surface);
                }
                if let Some(view) = sw.view.borrow().as_ref() {
                    view.preview.queue_draw();
                }
            });
        }
    }

    /// The best picture of `path` so far: the full preview, else the thumbnail.
    fn picture(&self, path: &Path) -> Option<cairo::ImageSurface> {
        self.previews.borrow().get(path).cloned().or_else(|| self.entries.borrow().iter().find(|e| e.path == path).and_then(|e| e.thumb.clone()))
    }

    fn draw_preview(&self, cr: &cairo::Context, w: f64, h: f64, s: f64) {
        let palette = theme::palette();
        let radius = 10.0 * s;
        let _ = cr.save();
        theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
        cr.clip();
        theme::set_source(cr, palette.recessed);
        let _ = cr.paint();
        let t = self.crossfade.borrow().value;
        if t < 1.0 {
            if let Some(old) = self.last_shown.borrow().as_ref().and_then(|p| self.picture(p)) {
                cover(cr, &old, w, h, 1.0);
            }
        }
        if let Some(new) = self.shown.borrow().as_ref().and_then(|p| self.picture(p)) {
            cover(cr, &new, w, h, t);
        }
        // A scrim at the foot for the caption.
        let foot = cairo::LinearGradient::new(0.0, h * 0.55, 0.0, h);
        foot.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
        foot.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.62);
        let _ = cr.set_source(&foot);
        let _ = cr.paint();
        let _ = cr.restore();
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius);
        cr.set_line_width(1.0);
        theme::set_source(cr, HAIR);
        let _ = cr.stroke();
    }
}

// ── Drawing helpers ─────────────────────────────────────────────────────────

fn surface(data: Vec<u8>, w: i32, h: i32) -> Option<cairo::ImageSurface> {
    cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, w * 4).ok()
}

/// Paints `image` to cover (w, h), centred, at `alpha`.
fn cover(cr: &cairo::Context, image: &cairo::ImageSurface, w: f64, h: f64, alpha: f64) {
    let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let k = (w / iw).max(h / ih);
    let _ = cr.save();
    cr.translate((w - iw * k) / 2.0, (h - ih * k) / 2.0);
    cr.scale(k, k);
    let _ = cr.set_source_surface(image, 0.0, 0.0);
    cr.source().set_filter(cairo::Filter::Good);
    let _ = cr.paint_with_alpha(alpha);
    let _ = cr.restore();
}

/// A rectangle with its top-left and bottom-right corners cut by `cut`.
fn chamfer(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, cut: f64) {
    cr.new_path();
    cr.move_to(x + cut, y);
    cr.line_to(x + w, y);
    cr.line_to(x + w, y + h - cut);
    cr.line_to(x + w - cut, y + h);
    cr.line_to(x, y + h);
    cr.line_to(x, y + cut);
    cr.close_path();
}

fn nearest_group(hue: f64) -> usize {
    let distance = |c: f64| {
        let d = (hue - c).rem_euclid(360.0);
        d.min(360.0 - d)
    };
    (0..GROUPS.len()).min_by(|&a, &b| distance(GROUPS[a].1).total_cmp(&distance(GROUPS[b].1))).unwrap_or(MONO)
}

/// A muted stand-in colour for a group.
fn group_colour(group: usize) -> Rgba {
    let Some(&(_, hue)) = GROUPS.get(group) else { return Rgba::rgb8(0x8a, 0x84, 0x7c) };
    let (s, v) = (0.5, 0.78);
    let c = v * s;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Rgba { r: r + m, g: g + m, b: b + m, a: 1.0 }
}

fn swatch(s: f64, group: usize, chosen: bool) -> gtk::DrawingArea {
    let size = (14.0 * s).round() as i32;
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).build();
    area.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        chamfer(cr, 1.0, 1.0, w - 2.0, h - 2.0, 4.0 * s);
        theme::set_source(cr, group_colour(group));
        let _ = cr.fill_preserve();
        cr.set_line_width(if chosen { 1.6 * s } else { 1.0 });
        theme::set_source(cr, if chosen { theme::palette().accent_lit } else { Rgba::rgba8(0, 0, 0, 0.45) });
        let _ = cr.stroke();
    });
    area
}

#[allow(clippy::too_many_arguments)]
fn draw_tile(cr: &cairo::Context, w: f64, h: f64, s: f64, entry: &Entry, selected: bool, hovered: bool, current: bool) {
    let palette = theme::palette();
    let cut = 12.0 * s;
    let inset = 2.0 * s;
    let (x, y, tw, th) = (inset, inset, w - 2.0 * inset, h - 2.0 * inset);
    let _ = cr.save();
    chamfer(cr, x, y, tw, th, cut);
    cr.clip();
    theme::set_source(cr, palette.recessed);
    let _ = cr.paint();
    if let Some(thumb) = &entry.thumb {
        cover(cr, thumb, w, h, 1.0);
    }
    // Others sit back so the pick reads.
    if !selected && !hovered {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.28);
        let _ = cr.paint();
    }
    // The group chip, bottom left.
    let chip = 7.0 * s;
    chamfer(cr, x + 7.0 * s, y + th - chip - 7.0 * s, chip, chip, 2.0 * s);
    theme::set_source(cr, group_colour(entry.group));
    let _ = cr.fill_preserve();
    cr.set_line_width(1.0);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
    let _ = cr.stroke();
    let _ = cr.restore();

    // The frame: accent for the selection, a light edge for hover, a hairline otherwise.
    chamfer(cr, x, y, tw, th, cut);
    let (edge, width) = if selected {
        (palette.accent_lit, 2.0 * s)
    } else if hovered {
        (CREAM.alpha(0.6), 1.2 * s)
    } else {
        (HAIR, 1.0)
    };
    cr.set_line_width(width);
    theme::set_source(cr, edge);
    let _ = cr.stroke();

    // On screen now: a dot in the uncut top-right corner.
    if wallpaper::is_video(&entry.path) {
        live_tag(cr, x + tw - 8.0 * s, y + th - 8.0 * s, s);
    }
    if current {
        let (cx, cy, r) = (x + tw - 9.0 * s, y + 9.0 * s, 3.5 * s);
        cr.arc(cx, cy, r + 1.5 * s, 0.0, 2.0 * PI);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        let _ = cr.fill();
        cr.arc(cx, cy, r, 0.0, 2.0 * PI);
        theme::set_source(cr, palette.accent_lit);
        let _ = cr.fill();
    }
}

/// A small "LIVE" tag with its bottom-right corner at (right, bottom).
fn live_tag(cr: &cairo::Context, right: f64, bottom: f64, s: f64) {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text("LIVE");
    layout.set_attributes(Some(&TextStyle::new(7.5 * s, pango::Weight::Bold, CREAM).mono().tracking(1.0 * s).attrs()));
    let (_, logical) = layout.pixel_extents();
    let (pad_x, pad_y) = (4.0 * s, 1.5 * s);
    let (w, h) = (f64::from(logical.width()) + 2.0 * pad_x, f64::from(logical.height()) + 2.0 * pad_y);
    theme::rounded_rect(cr, right - w, bottom - h, w, h, 3.0 * s);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
    let _ = cr.fill_preserve();
    cr.set_line_width(1.0);
    theme::set_source(cr, theme::palette().accent.alpha(0.8));
    let _ = cr.stroke();
    theme::set_source(cr, CREAM);
    cr.move_to(right - w + pad_x, bottom - h + pad_y);
    pangocairo::functions::show_layout(cr, &layout);
}

/// "ON SCREEN" with a dot, for the preview caption.
fn on_screen_tag(s: f64) -> gtk::Overlay {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (6.0 * s).round() as i32);
    row.set_margin_start((8.0 * s).round() as i32);
    row.set_margin_end((9.0 * s).round() as i32);
    row.set_margin_top((3.0 * s).round() as i32);
    row.set_margin_bottom((3.0 * s).round() as i32);
    let dot = gtk::DrawingArea::builder().content_width((7.0 * s).round() as i32).content_height((7.0 * s).round() as i32).valign(gtk::Align::Center).build();
    dot.set_draw_func(|_, cr, w, h| {
        cr.arc(f64::from(w) / 2.0, f64::from(h) / 2.0, f64::from(w.min(h)) / 2.0, 0.0, 2.0 * PI);
        theme::set_source(cr, theme::palette().accent_lit);
        let _ = cr.fill();
    });
    row.append(&dot);
    row.append(&TextStyle::new(8.5 * s, pango::Weight::Bold, CREAM).mono().tracking(1.4 * s).label("ON SCREEN"));
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, (f64::from(h) - 1.0) / 2.0);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, theme::palette().accent.alpha(0.7));
        let _ = cr.stroke();
    });
    let tag = gtk::Overlay::new();
    tag.set_child(Some(&back));
    tag.add_overlay(&row);
    tag.set_measure_overlay(&row, true);
    tag
}
