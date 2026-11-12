//! The wallpaper browser, `eclipse-shell walls`: an app window of its own
//! process. Search wallhaven, MotionBGs and MoeWalls, or browse the library,
//! on the left; preview the pick on the right (live ones play), download it
//! or set it. Library pictures can be colour-tuned (wallust options per
//! image) or adjusted (a graded copy).
//!
//! Setting goes through the running shell (`eclipse-shell wallpaper-set`),
//! which owns the wallpaper and the live wallpaper process.

mod adjust;
mod sources;
mod tune;
mod ui;
mod video;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, BRIGHT, CREAM, DIM, FAINT, HAIR};
use crate::{config, hyprland, mark, popout, wallpaper};
use sources::{Item, Kind, Query, Source};
use video::Player;

const APP_ID: &str = "io.github.bjthompson805.EclipseWalls";
const TILE_W: f64 = 166.0;
const TILE_H: f64 = 100.0;
const SIDE_W: f64 = 440.0;

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    let shown: Rc<RefCell<Option<Rc<Walls>>>> = Rc::default();
    app.connect_activate(move |app| {
        if let Some(walls) = shown.borrow().as_ref() {
            walls.window.present();
            return;
        }
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(crate::TRANSPARENT_WINDOW);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        theme::watch_palette();
        *shown.borrow_mut() = Some(Walls::new(app));
    });
    app.run_with_args::<&str>(&[])
}

pub struct Walls {
    window: gtk::ApplicationWindow,
    s: f64,
    source: Cell<Source>,
    query: RefCell<Query>,
    items: RefCell<Vec<Item>>,
    selected: Cell<Option<usize>>,
    /// Bumped per search, so a slow page of an old one is dropped.
    generation: Cell<u32>,
    loading: Cell<bool>,
    /// No more pages.
    exhausted: Cell<bool>,
    tabs: gtk::Box,
    toolbar: gtk::Box,
    grid: gtk::FlowBox,
    grid_area: gtk::Box,
    side: gtk::Box,
    status: gtk::Label,
    /// Pictures by URL or path: thumbnails, then full previews.
    thumbs: RefCell<HashMap<String, cairo::ImageSurface>>,
    previews: RefCell<HashMap<String, cairo::ImageSurface>>,
    /// The playing clip's latest frame.
    frame: RefCell<Option<cairo::ImageSurface>>,
    player: Rc<Player>,
    preview: RefCell<Option<gtk::DrawingArea>>,
    downloading: Cell<bool>,
    /// Armed by a first click on Delete.
    confirm_delete: Cell<bool>,
    /// The open tool (Tune or Adjust), held while it shows.
    tool: RefCell<Option<Rc<dyn std::any::Any>>>,
    screen: (i32, i32),
}

impl Walls {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let monitor = gdk::Display::default().and_then(|d| d.monitors().item(0)).and_downcast::<gdk::Monitor>();
        let (sw, sh) = monitor.as_ref().map_or((1920, 1080), |m| (m.geometry().width(), m.geometry().height()));
        let s = config::bar_scale(sh);
        let (ww, wh) = ((f64::from(sw) * 0.8).min(1560.0) as i32, (f64::from(sh) * 0.8).min(980.0) as i32);
        hyprland::eval(&format!("hl.window_rule({{ name = \"eclipse-walls\", match = {{ class = \"^({APP_ID})$\" }}, float = true, size = {{ {ww}, {wh} }}, center = true }})"));

        let window = gtk::ApplicationWindow::builder().application(app).title("Walls").default_width(ww).default_height(wh).decorated(false).css_classes(["eclipse-shell"]).build();
        let px = |v: f64| (v * s).round() as i32;

        let back = gtk::DrawingArea::new();
        back.set_draw_func(|_, cr, _, _| {
            theme::set_source(cr, theme::palette().base);
            let _ = cr.paint();
        });
        theme::on_palette_change({
            let back = back.downgrade();
            move || back.upgrade().into_iter().for_each(|b| b.queue_draw())
        });

        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, px(12.0));
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(8)
            .column_spacing(px(10.0) as u32)
            .row_spacing(px(10.0) as u32)
            .valign(gtk::Align::Start)
            .build();
        let grid_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid_area.set_vexpand(true);
        let side = gtk::Box::new(gtk::Orientation::Vertical, px(12.0));
        side.set_size_request(px(SIDE_W), -1);
        let status = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label("");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(pango::EllipsizeMode::End);

        let walls = Rc::new(Self {
            window,
            s,
            source: Cell::new(Source::Wallhaven),
            query: RefCell::new(Query { text: String::new(), page: 1, top: true, fit: Some((sw, sh)), kind: Kind::All }),
            items: RefCell::default(),
            selected: Cell::new(None),
            generation: Cell::new(0),
            loading: Cell::new(false),
            exhausted: Cell::new(false),
            tabs,
            toolbar,
            grid,
            grid_area,
            side,
            status,
            thumbs: RefCell::default(),
            previews: RefCell::default(),
            frame: RefCell::default(),
            player: Player::new(),
            preview: RefCell::default(),
            downloading: Cell::new(false),
            confirm_delete: Cell::new(false),
            tool: RefCell::default(),
            screen: (sw, sh),
        });

        // Left: title and tabs, toolbar, grid. Right: the preview column.
        let left = gtk::Box::new(gtk::Orientation::Vertical, px(14.0));
        left.set_hexpand(true);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        head.append(&mark::mark(18.0 * s));
        head.append(&TextStyle::new(15.0 * s, pango::Weight::Bold, BRIGHT).label("Walls"));
        let what = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label("Wallpaper Browser");
        what.set_valign(gtk::Align::Baseline);
        head.append(&what);
        let gap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        gap.set_hexpand(true);
        head.append(&gap);
        head.append(&walls.tabs);
        left.append(&head);
        left.append(&walls.toolbar);
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&walls.grid).vexpand(true).build();
        scroller.connect_edge_reached({
            let walls = Rc::downgrade(&walls);
            move |_, edge| {
                if edge == gtk::PositionType::Bottom {
                    walls.upgrade().into_iter().for_each(|w| w.more());
                }
            }
        });
        walls.grid_area.append(&scroller);
        left.append(&walls.grid_area);
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        foot.append(&walls.status);
        let folders = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label("~/Pictures/Wallpapers · ~/Pictures/livewalls");
        foot.append(&folders);
        left.append(&foot);

        let body = gtk::Box::new(gtk::Orientation::Horizontal, px(18.0));
        body.set_margin_start(px(20.0));
        body.set_margin_end(px(20.0));
        body.set_margin_top(px(18.0));
        body.set_margin_bottom(px(14.0));
        body.append(&left);
        body.append(&ui::rule_v());
        // Held at its width: a long name or a row of chips would otherwise
        // widen the column at the grid's expense.
        let side_frame = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&walls.side).width_request(px(SIDE_W)).build();
        let right = gtk::Box::new(gtk::Orientation::Vertical, px(8.0));
        let close_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        close_row.set_halign(gtk::Align::End);
        close_row.append(&crate::chrome::close_button(s, &walls.window));
        right.append(&close_row);
        side_frame.set_vexpand(true);
        right.append(&side_frame);
        body.append(&right);

        let layers = gtk::Overlay::new();
        layers.set_child(Some(&back));
        layers.add_overlay(&body);
        walls.window.set_child(Some(&layers));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let window = walls.window.downgrade();
            move |_, key, _, modifiers| {
                if key == gdk::Key::Escape || (key == gdk::Key::q && modifiers.contains(gdk::ModifierType::CONTROL_MASK)) {
                    window.upgrade().into_iter().for_each(|w| w.close());
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        walls.window.add_controller(keys);
        walls.window.connect_close_request({
            let player = walls.player.clone();
            move |_| {
                player.stop();
                glib::Propagation::Proceed
            }
        });

        walls.show_tabs();
        walls.show_toolbar();
        walls.show_side();
        walls.search();
        walls.window.present();
        walls
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    // ── Tabs and toolbar ────────────────────────────────────────────────────

    fn show_tabs(self: &Rc<Self>) {
        while let Some(c) = self.tabs.first_child() {
            self.tabs.remove(&c);
        }
        let chosen = Source::ALL.iter().position(|&x| x == self.source.get()).unwrap_or(0);
        let names: Vec<&str> = Source::ALL.iter().map(|x| x.name()).collect();
        let walls = Rc::downgrade(self);
        self.tabs.append(&ui::segmented(self.s, &names, chosen, move |i| {
            if let Some(w) = walls.upgrade() {
                w.source.set(Source::ALL[i]);
                w.query.borrow_mut().text.clear();
                w.show_tabs();
                w.show_toolbar();
                w.search();
            }
        }));
    }

    fn show_toolbar(self: &Rc<Self>) {
        let s = self.s;
        while let Some(c) = self.toolbar.first_child() {
            self.toolbar.remove(&c);
        }
        let source = self.source.get();
        let placeholder = match source {
            Source::Wallhaven => "Search wallhaven (Enter)",
            Source::MotionBgs => "A tag, like anime or nature (Enter)",
            Source::MoeWalls => "Search MoeWalls (Enter)",
            Source::Library => "Filter by name",
        };
        let (field, text) = ui::field(s, "search", placeholder);
        text.set_text(&self.query.borrow().text);
        let walls = Rc::downgrade(self);
        if source.remote() {
            text.connect_activate(move |t| {
                if let Some(w) = walls.upgrade() {
                    w.query.borrow_mut().text = t.text().trim().to_owned();
                    w.search();
                }
            });
        } else {
            text.connect_changed(move |t| {
                if let Some(w) = walls.upgrade() {
                    w.query.borrow_mut().text = t.text().trim().to_owned();
                    w.search();
                }
            });
        }
        self.toolbar.append(&field);
        match source {
            Source::Wallhaven => {
                let query = self.query.borrow().clone();
                let walls = Rc::downgrade(self);
                self.toolbar.append(&ui::segmented(s, &["Top month", "Latest"], usize::from(!query.top), move |i| {
                    if let Some(w) = walls.upgrade() {
                        w.query.borrow_mut().top = i == 0;
                        w.show_toolbar();
                        w.search();
                    }
                }));
                let walls = Rc::downgrade(self);
                let screen = self.screen;
                let fit = query.fit.is_some();
                let toggle = gtk::Box::new(gtk::Orientation::Horizontal, (6.0 * s).round() as i32);
                toggle.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label("Fit screen"));
                toggle.append(&popout::switch(s, fit, true, move || {
                    if let Some(w) = walls.upgrade() {
                        w.query.borrow_mut().fit = if fit { None } else { Some(screen) };
                        w.show_toolbar();
                        w.search();
                    }
                }));
                toggle.set_valign(gtk::Align::Center);
                self.toolbar.append(&toggle);
            }
            Source::Library => {
                let kind = self.query.borrow().kind;
                let walls = Rc::downgrade(self);
                let chosen = match kind {
                    Kind::All => 0,
                    Kind::Images => 1,
                    Kind::Live => 2,
                };
                self.toolbar.append(&ui::segmented(s, &["All", "Images", "Live"], chosen, move |i| {
                    if let Some(w) = walls.upgrade() {
                        w.query.borrow_mut().kind = [Kind::All, Kind::Images, Kind::Live][i];
                        w.show_toolbar();
                        w.search();
                    }
                }));
            }
            _ => {}
        }
    }

    // ── Searching and the grid ──────────────────────────────────────────────

    /// Starts over on page one.
    fn search(self: &Rc<Self>) {
        self.query.borrow_mut().page = 1;
        self.exhausted.set(false);
        self.items.borrow_mut().clear();
        self.selected.set(None);
        while let Some(c) = self.grid.first_child() {
            self.grid.remove(&c);
        }
        self.show_side();
        self.fetch();
    }

    /// The next page, as the grid scrolls to its end.
    fn more(self: &Rc<Self>) {
        if self.loading.get() || self.exhausted.get() || !self.source.get().remote() {
            return;
        }
        self.query.borrow_mut().page += 1;
        self.fetch();
    }

    fn fetch(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.loading.set(true);
        let (source, query) = (self.source.get(), self.query.borrow().clone());
        self.set_status(&format!("Loading {}…", source.name()));
        let walls = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || sources::search(source, &query)).await.unwrap_or_else(|_| Err("search failed".into()));
            let Some(w) = walls.upgrade().filter(|w| w.generation.get() == generation) else { return };
            w.loading.set(false);
            match result {
                Ok(found) => {
                    if found.is_empty() {
                        w.exhausted.set(true);
                    }
                    let start = w.items.borrow().len();
                    // Pages can overlap as new posts push old ones along.
                    let known: std::collections::HashSet<String> = w.items.borrow().iter().map(|i| i.id.clone()).collect();
                    let fresh: Vec<Item> = found.into_iter().filter(|i| !known.contains(&i.id)).collect();
                    w.items.borrow_mut().extend(fresh);
                    let total = w.items.borrow().len();
                    for index in start..total {
                        w.add_tile(index);
                    }
                    w.set_status(&match (total, w.source.get()) {
                        (0, Source::Library) => "Nothing in the library matches".to_owned(),
                        (0, _) => "Nothing found".to_owned(),
                        (n, Source::Library) => format!("{n} in the library"),
                        (n, s) => format!("{n} from {}", s.name()),
                    });
                    if total == 0 {
                        w.show_side();
                    }
                }
                Err(why) => w.set_status(&why),
            }
        });
    }

    fn add_tile(self: &Rc<Self>, index: usize) {
        let s = self.s;
        let Some(item) = self.items.borrow().get(index).cloned() else { return };
        let tile = gtk::DrawingArea::builder().content_width((TILE_W * s).round() as i32).content_height((TILE_H * s).round() as i32).build();
        let hovered = Rc::new(Cell::new(false));
        let saved = item.source.remote() && item.saved();
        tile.set_draw_func({
            let (walls, hovered, key) = (Rc::downgrade(self), hovered.clone(), item.thumb.clone());
            move |_, cr, w, h| {
                let Some(walls) = walls.upgrade() else { return };
                let picture = walls.thumbs.borrow().get(&key).cloned();
                draw_tile(cr, f64::from(w), f64::from(h), s, picture.as_ref(), item.live, saved, walls.selected.get() == Some(index), hovered.get());
            }
        });
        popout::on_hover(&tile, {
            let tile = tile.downgrade();
            move |inside| {
                hovered.set(inside);
                tile.upgrade().into_iter().for_each(|t| t.queue_draw());
            }
        });
        let walls = Rc::downgrade(self);
        popout::on_click(&tile, move || {
            if let Some(w) = walls.upgrade() {
                w.select(index);
            }
        });
        self.grid.append(&tile);

        let referer = self.items.borrow()[index].referer();
        let thumb = self.items.borrow()[index].thumb.clone();
        let height = (TILE_H * s * 2.0).round() as i32;
        let walls = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let key = thumb.clone();
            let decoded = gio::spawn_blocking(move || {
                let file = sources::fetch_picture(&thumb, referer, "thumbs")?;
                wallpaper::decode_scaled(file.to_str()?, height)
            })
            .await
            .ok()
            .flatten();
            let (Some(w), Some((data, dw, dh))) = (walls.upgrade(), decoded) else { return };
            if let Ok(surface) = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, dw, dh, dw * 4) {
                w.thumbs.borrow_mut().insert(key, surface);
            }
            tile.queue_draw();
        });
    }

    fn redraw_grid(&self) {
        let mut child = self.grid.first_child();
        while let Some(c) = child {
            if let Some(area) = c.first_child() {
                area.queue_draw();
            }
            child = c.next_sibling();
        }
    }

    fn select(self: &Rc<Self>, index: usize) {
        self.selected.set(Some(index));
        self.confirm_delete.set(false);
        self.redraw_grid();
        self.show_side();
    }

    fn current(&self) -> Option<Item> {
        self.selected.get().and_then(|i| self.items.borrow().get(i).cloned())
    }

    // ── The preview column ──────────────────────────────────────────────────

    fn clear_side(&self) {
        self.player.stop();
        *self.tool.borrow_mut() = None;
        *self.frame.borrow_mut() = None;
        *self.preview.borrow_mut() = None;
        while let Some(c) = self.side.first_child() {
            self.side.remove(&c);
        }
    }

    fn show_side(self: &Rc<Self>) {
        self.clear_side();
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        let Some(item) = self.current() else {
            self.side.append(&ui::note(s, "wallpaper", "Pick a wallpaper to preview it here."));
            return;
        };

        let (pw, ph) = self.preview_size();
        let area = gtk::DrawingArea::builder().content_width(pw).content_height(ph).build();
        area.set_draw_func({
            let (walls, key) = (Rc::downgrade(self), item.thumb.clone());
            let preview_key = item.preview.clone();
            move |_, cr, w, h| {
                let Some(walls) = walls.upgrade() else { return };
                let picture = walls.frame.borrow().clone().or_else(|| walls.previews.borrow().get(&preview_key).cloned()).or_else(|| walls.thumbs.borrow().get(&key).cloned());
                draw_preview(cr, f64::from(w), f64::from(h), s, picture.as_ref());
            }
        });
        self.side.append(&area);
        *self.preview.borrow_mut() = Some(area.clone());
        self.load_preview(&item, pw);

        // Name, then size, source and kind.
        let name = TextStyle::new(14.0 * s, pango::Weight::Semibold, BRIGHT).label(&item.name);
        name.set_xalign(0.0);
        name.set_wrap(true);
        name.set_max_width_chars(30);
        self.side.append(&name);
        let mut facts = vec![item.source.name().to_uppercase()];
        if !item.resolution.is_empty() {
            facts.insert(0, item.resolution.replace('x', " × "));
        }
        if item.live {
            facts.push("LIVE".into());
        }
        if item.source.remote() && item.saved() {
            facts.push("IN LIBRARY".into());
        }
        let detail = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).mono().tracking(0.8 * s).label(&facts.join(" · "));
        detail.set_xalign(0.0);
        self.side.append(&detail);
        if item.local.is_some() && item.resolution.is_empty() {
            // The library knows the size only from the file.
            let path = item.local.clone().unwrap_or_default();
            let detail = detail.clone();
            let live = item.live;
            glib::spawn_future_local(async move {
                if let Ok(Some((w, h))) = gio::spawn_blocking(move || wallpaper::size(&path)).await {
                    detail.set_text(&format!("{w} × {h} · LIBRARY{}", if live { " · LIVE" } else { "" }));
                }
            });
        }

        // Two to a row in even columns, so the buttons never widen the column.
        let actions = gtk::Grid::builder().column_spacing(px(8.0)).row_spacing(px(8.0)).column_homogeneous(true).build();
        let mut placed = 0;
        let mut place = |button: gtk::Overlay| {
            actions.attach(&button, placed % 2, placed / 2, 1, 1);
            placed += 1;
        };
        let busy = self.downloading.get();
        let walls = Rc::downgrade(self);
        if item.source.remote() {
            let saved = item.saved();
            let w2 = walls.clone();
            place(ui::button(s, Some("wallpaper"), "Set as wallpaper", true, !busy, move || {
                if let Some(w) = w2.upgrade() {
                    w.download(true);
                }
            }));
            place(ui::button(s, Some(if saved { "check" } else { "download" }), if saved { "In library" } else { "Download" }, false, !busy && !saved, move || {
                if let Some(w) = walls.upgrade() {
                    w.download(false);
                }
            }));
        } else {
            let path = item.local.clone().unwrap_or_default();
            let set_path = path.clone();
            place(ui::button(s, Some("wallpaper"), "Set as wallpaper", true, true, {
                let walls = walls.clone();
                move || {
                    if let Some(w) = walls.upgrade() {
                        w.set_wallpaper(&set_path);
                    }
                }
            }));
            place(ui::button(s, Some("palette"), "Tune colours", false, true, {
                let (walls, path) = (walls.clone(), path.clone());
                move || {
                    if let Some(w) = walls.upgrade() {
                        w.clear_side();
                        tune::show(&w, &path);
                    }
                }
            }));
            place(ui::button(s, Some("tune"), "Adjust", false, !item.live, {
                let (walls, path) = (walls.clone(), path.clone());
                move || {
                    if let Some(w) = walls.upgrade() {
                        w.clear_side();
                        adjust::show(&w, &path);
                    }
                }
            }));
            let armed = self.confirm_delete.get();
            place(ui::button(s, Some("delete"), if armed { "Click again to delete" } else { "Delete" }, false, true, move || {
                if let Some(w) = walls.upgrade() {
                    w.delete(&path);
                }
            }));
        }
        self.side.append(&actions);
    }

    /// The preview's size: the column's width, the screen's shape.
    fn preview_size(&self) -> (i32, i32) {
        let w = SIDE_W * self.s;
        (w.round() as i32, (w * f64::from(self.screen.1) / f64::from(self.screen.0)).round() as i32)
    }

    fn load_preview(self: &Rc<Self>, item: &Item, width: i32) {
        let device = self.window.scale_factor().max(1);
        if item.live {
            let source = item.local.as_ref().map_or_else(|| item.preview.clone(), |p| p.display().to_string());
            let walls = Rc::downgrade(self);
            self.player.play(&source, item.referer(), width * device, move |frame| {
                if let Some(w) = walls.upgrade() {
                    *w.frame.borrow_mut() = Some(frame);
                    if let Some(area) = w.preview.borrow().as_ref() {
                        area.queue_draw();
                    }
                }
            });
            return;
        }
        if self.previews.borrow().contains_key(&item.preview) {
            return;
        }
        let (url, referer) = (item.preview.clone(), item.referer());
        let height = (f64::from(self.preview_size().1) * f64::from(device)).round() as i32;
        let walls = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let key = url.clone();
            let decoded = gio::spawn_blocking(move || {
                let file = sources::fetch_picture(&url, referer, "full")?;
                wallpaper::decode_scaled(file.to_str()?, height)
            })
            .await
            .ok()
            .flatten();
            let (Some(w), Some((data, dw, dh))) = (walls.upgrade(), decoded) else { return };
            if let Ok(surface) = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, dw, dh, dw * 4) {
                w.previews.borrow_mut().insert(key, surface);
            }
            if let Some(area) = w.preview.borrow().as_ref() {
                area.queue_draw();
            }
        });
    }

    // ── Actions ─────────────────────────────────────────────────────────────

    fn download(self: &Rc<Self>, then_set: bool) {
        let Some(item) = self.current() else { return };
        if self.downloading.replace(true) {
            return;
        }
        self.show_side();
        let progress = Arc::new(AtomicU64::new(0));
        let ticker = {
            let (walls, progress) = (Rc::downgrade(self), progress.clone());
            glib::timeout_add_local(Duration::from_millis(250), move || {
                let Some(w) = walls.upgrade().filter(|w| w.downloading.get()) else { return glib::ControlFlow::Break };
                let mb = progress.load(Ordering::Relaxed) as f64 / 1_048_576.0;
                w.set_status(&format!("Downloading {}… {mb:.1} MB", item_name(&w)));
                glib::ControlFlow::Continue
            })
        };
        let walls = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let job = item.clone();
            let result = gio::spawn_blocking(move || sources::download(&job, &progress)).await.unwrap_or_else(|_| Err("the download failed".into()));
            let Some(w) = walls.upgrade() else { return };
            w.downloading.set(false);
            drop(ticker);
            match result {
                Ok(path) => {
                    w.set_status(&format!("Saved {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    if then_set {
                        w.set_wallpaper(&path);
                    }
                }
                Err(why) => w.set_status(&why),
            }
            w.redraw_grid();
            if w.current().is_some_and(|c| c.id == item.id) {
                w.show_side();
            }
        });
    }

    fn set_wallpaper(&self, path: &std::path::Path) {
        let shell = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eclipse-shell"));
        match std::process::Command::new(shell).arg("wallpaper-set").arg(path).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.set_status(&format!("Set {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
            Err(e) => self.set_status(&format!("Couldn't reach the shell: {e}")),
        }
    }

    fn delete(self: &Rc<Self>, path: &std::path::Path) {
        if !self.confirm_delete.replace(true) {
            self.show_side();
            // Disarm after a moment.
            let walls = Rc::downgrade(self);
            glib::timeout_add_local_once(Duration::from_secs(3), move || {
                if let Some(w) = walls.upgrade().filter(|w| w.confirm_delete.get()) {
                    w.confirm_delete.set(false);
                    w.show_side();
                }
            });
            return;
        }
        self.confirm_delete.set(false);
        match sources::trash(path) {
            Ok(()) => {
                self.set_status("Moved to the trash");
                self.search();
            }
            Err(why) => self.set_status(&format!("Couldn't delete: {why}")),
        }
    }

    /// Back from a tool to the preview, re-reading the library when a tool
    /// wrote a new file.
    fn back(self: &Rc<Self>, select: Option<PathBuf>) {
        if let Some(path) = select {
            self.search();
            let walls = Rc::downgrade(self);
            // Select the new file once the library is listed.
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                if let Some(w) = walls.upgrade() {
                    let found = w.items.borrow().iter().position(|i| i.local.as_ref() == Some(&path));
                    if let Some(i) = found {
                        w.select(i);
                    }
                }
            });
        } else {
            self.show_side();
        }
    }
}

fn item_name(w: &Walls) -> String {
    w.current().map(|i| i.name).unwrap_or_default()
}

/// Paints `image` to cover (w, h), centred.
fn cover(cr: &cairo::Context, image: &cairo::ImageSurface, w: f64, h: f64) {
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
    let _ = cr.paint();
    let _ = cr.restore();
}

#[allow(clippy::too_many_arguments)]
fn draw_tile(cr: &cairo::Context, w: f64, h: f64, s: f64, picture: Option<&cairo::ImageSurface>, live: bool, saved: bool, selected: bool, hovered: bool) {
    let palette = theme::palette();
    let r = 8.0 * s;
    let _ = cr.save();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, r);
    cr.clip();
    theme::set_source(cr, palette.recessed);
    let _ = cr.paint();
    if let Some(p) = picture {
        cover(cr, p, w, h);
    }
    if !selected && !hovered {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.15);
        let _ = cr.paint();
    }
    let _ = cr.restore();
    if live {
        tag(cr, w - 7.0 * s, h - 7.0 * s, s, "LIVE");
    }
    if saved {
        // A check in a dot, top right.
        let (cx, cy, rr) = (w - 13.0 * s, 13.0 * s, 7.0 * s);
        cr.arc(cx, cy, rr, 0.0, 2.0 * std::f64::consts::PI);
        theme::set_source(cr, palette.accent);
        let _ = cr.fill();
        cr.set_line_width(1.6 * s);
        theme::set_source(cr, BRIGHT);
        cr.move_to(cx - 3.2 * s, cy + 0.2 * s);
        cr.line_to(cx - 0.8 * s, cy + 2.6 * s);
        cr.line_to(cx + 3.4 * s, cy - 2.4 * s);
        let _ = cr.stroke();
    }
    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, r);
    let (edge, width) = if selected {
        (palette.accent_lit, 2.0 * s)
    } else if hovered {
        (CREAM.alpha(0.55), 1.2 * s)
    } else {
        (HAIR, 1.0)
    };
    cr.set_line_width(width);
    theme::set_source(cr, edge);
    let _ = cr.stroke();
}

fn draw_preview(cr: &cairo::Context, w: f64, h: f64, s: f64, picture: Option<&cairo::ImageSurface>) {
    let palette = theme::palette();
    let r = 10.0 * s;
    let _ = cr.save();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, r);
    cr.clip();
    theme::set_source(cr, palette.recessed);
    let _ = cr.paint();
    if let Some(p) = picture {
        cover(cr, p, w, h);
    }
    let _ = cr.restore();
    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, r);
    cr.set_line_width(1.0);
    theme::set_source(cr, HAIR);
    let _ = cr.stroke();
}

/// A small caps tag with its bottom-right corner at (right, bottom).
fn tag(cr: &cairo::Context, right: f64, bottom: f64, s: f64, text: &str) {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text(text);
    layout.set_attributes(Some(&TextStyle::new(7.5 * s, pango::Weight::Bold, CREAM).mono().tracking(1.0 * s).attrs()));
    let (_, logical) = layout.pixel_extents();
    let (pad_x, pad_y) = (4.0 * s, 1.5 * s);
    let (tw, th) = (f64::from(logical.width()) + 2.0 * pad_x, f64::from(logical.height()) + 2.0 * pad_y);
    theme::rounded_rect(cr, right - tw, bottom - th, tw, th, 3.0 * s);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
    let _ = cr.fill_preserve();
    cr.set_line_width(1.0);
    theme::set_source(cr, theme::palette().accent.alpha(0.8));
    let _ = cr.stroke();
    theme::set_source(cr, CREAM);
    cr.move_to(right - tw + pad_x, bottom - th + pad_y);
    pangocairo::functions::show_layout(cr, &layout);
}
