//! App launcher: a full-screen Overlay-layer surface that takes the keyboard,
//! blurs and dims the desktop, and shows a card with a search row over either
//! the resting dashboard (empty query) or ranked app results.
//!
//! The grainy blur behind it is Hyprland's own: a layer rule turns on blur for
//! this surface, and while the launcher is open the blur size is raised to
//! `LAUNCHER_BLUR` (one pass at that size gives the grain), then restored.

mod dashboard;
mod results;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::anim::{self, Easing, Tween, MORPH};
use crate::config;
use crate::hyprland;
use crate::mark;
use crate::theme::{self, TextStyle, CHIP_FILL, CREAM, FAINT, HAIR, ICON_DIM, SCRIM};
use results::ResultList;

const NAMESPACE: &str = "eclipse-shell-launcher";
const OPEN_MS: f64 = 240.0;
const RESIZE_MS: f64 = 420.0;
const CLOSED_SCALE: f64 = 0.92;

/// Everything sized for one monitor scale; rebuilt when the scale changes.
struct Card {
    s: f64,
    width: f64,
    background: gtk::DrawingArea,
    column: gtk::Box,
    query: gtk::Text,
    placeholder: gtk::Label,
    counter: gtk::Label,
    dashboard: gtk::Overlay,
    list: Rc<ResultList>,
    empty: gtk::Label,
}

pub struct Launcher {
    window: gtk::ApplicationWindow,
    scrim: gtk::DrawingArea,
    stage: gtk::Fixed,
    card: RefCell<Option<Card>>,
    apps: RefCell<Vec<gio::AppInfo>>,
    /// Screen size, from the scrim's allocation.
    screen: Cell<(f64, f64)>,
    /// Card's top-left on screen.
    origin: Cell<(f64, f64)>,
    /// 0 closed .. 1 open.
    openness: Rc<RefCell<Tween>>,
    height: Rc<RefCell<Tween>>,
    open: Cell<bool>,
    /// The compositor blur to put back on close: (enabled, size).
    /// The launcher is keeping Hyprland's blur on; see [`hyprland::hold_blur`].
    holding_blur: Cell<bool>,
    monitor: RefCell<Option<gdk::Monitor>>,
}

impl Launcher {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some(NAMESPACE));
        window.set_layer(Layer::Overlay);
        // Held until the window unmaps, so the keyboard returns cleanly to
        // the focused app after the close animation.
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);

        let scrim = gtk::DrawingArea::new();
        let stage = gtk::Fixed::new();
        let layers = gtk::Overlay::new();
        layers.set_child(Some(&scrim));
        layers.add_overlay(&stage);
        window.set_child(Some(&layers));

        hyprland::eval(&format!(
            "hl.layer_rule({{ name = \"{NAMESPACE}-blur\", match = {{ namespace = \"{NAMESPACE}\" }}, blur = true, no_anim = true }})"
        ));

        let launcher = Rc::new(Self {
            window,
            scrim,
            stage,
            card: RefCell::default(),
            apps: RefCell::default(),
            screen: Cell::new((0.0, 0.0)),
            origin: Cell::new((0.0, 0.0)),
            openness: Tween::shared(OPEN_MS, MORPH),
            height: Tween::shared(RESIZE_MS, MORPH),
            open: Cell::new(false),
            holding_blur: Cell::new(false),
            monitor: RefCell::default(),
        });

        launcher.scrim.set_draw_func({
            let launcher = Rc::downgrade(&launcher);
            move |_, cr, _, _| {
                let Some(launcher) = launcher.upgrade() else { return };
                let t = Easing::OutCubic.apply(launcher.openness.borrow().value);
                theme::set_source(cr, SCRIM.alpha(SCRIM.a * t));
                let _ = cr.paint();
            }
        });
        launcher.scrim.connect_resize({
            let launcher = Rc::downgrade(&launcher);
            move |_, w, h| {
                if let Some(launcher) = launcher.upgrade() {
                    launcher.screen.set((f64::from(w), f64::from(h)));
                    launcher.place();
                }
            }
        });
        launcher.connect_input();
        launcher
    }

    /// Which monitor to open on next; the bar keeps this pointed at the
    /// focused one.
    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        *self.monitor.borrow_mut() = monitor;
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.open.get() { self.hide() } else { self.show() }
    }

    fn show(self: &Rc<Self>) {
        let monitor = self.monitor.borrow().clone();
        let height = monitor.as_ref().map_or(1080, |m| m.geometry().height());
        let s = config::launcher_scale(height);
        self.window.set_monitor(monitor.as_ref());
        if self.card.borrow().as_ref().is_none_or(|c| c.s != s) {
            self.build_card(s);
        }

        let mut apps: Vec<_> = gio::AppInfo::all().into_iter().filter(|a| a.should_show()).collect();
        apps.sort_by_cached_key(|a| a.display_name().to_lowercase());
        *self.apps.borrow_mut() = apps;

        self.open.set(true);
        if let Some(card) = self.card.borrow().as_ref() {
            card.query.set_text("");
        }
        self.refresh();
        self.height.borrow_mut().jump(self.natural_height());
        self.force_blur();
        self.window.present();
        if let Some(card) = self.card.borrow().as_ref() {
            card.query.grab_focus();
        }
        self.animate_openness(1.0);
    }

    fn hide(self: &Rc<Self>) {
        if !self.open.replace(false) {
            return;
        }
        self.animate_openness(0.0);
    }

    fn animate_openness(self: &Rc<Self>, target: f64) {
        let launcher = Rc::downgrade(self);
        anim::animate_to(&self.stage, &self.openness, target, move |_, t| {
            let Some(launcher) = launcher.upgrade() else { return };
            launcher.place();
            launcher.scrim.queue_draw();
            // Fully closed: drop the window and give the blur back.
            if t <= 0.0 && !launcher.open.get() {
                launcher.window.set_visible(false);
                launcher.restore_blur();
            }
        });
    }

    fn force_blur(&self) {
        if config::LAUNCHER_BLUR > 0 && !self.holding_blur.replace(true) {
            hyprland::hold_blur(config::LAUNCHER_BLUR);
        }
    }

    fn restore_blur(&self) {
        if self.holding_blur.replace(false) {
            hyprland::release_blur();
        }
    }

    /// Positions the card: centred, a third of the way down (measured from
    /// its resting height so typing grows it downward), scaled and faded by
    /// how open the launcher is.
    fn place(&self) {
        let card = self.card.borrow();
        let Some(card) = card.as_ref() else { return };
        let (screen_w, screen_h) = self.screen.get();
        let resting = resting_height(card.s);
        let x = ((screen_w - card.width) / 2.0).round();
        let y = ((screen_h - resting) * 0.32).round();
        self.origin.set((x, y));

        let t = self.openness.borrow().value;
        let scale = CLOSED_SCALE + (1.0 - CLOSED_SCALE) * t;
        // Grow from the top edge's midpoint, where the search row sits.
        let transform = gsk::Transform::new()
            .translate(&graphene::Point::new((x + card.width * (1.0 - scale) / 2.0) as f32, y as f32))
            .scale(scale as f32, scale as f32);
        for widget in [card.background.upcast_ref::<gtk::Widget>(), card.column.upcast_ref()] {
            self.stage.set_child_transform(widget, Some(&transform));
            widget.set_opacity(Easing::OutCubic.apply(t));
        }
        let height = self.height.borrow().value.round() as i32;
        card.background.set_size_request(card.width.round() as i32, height);
    }

    fn natural_height(&self) -> f64 {
        let card = self.card.borrow();
        let Some(card) = card.as_ref() else { return 0.0 };
        let (_, natural, _, _) = card.column.measure(gtk::Orientation::Vertical, card.width.round() as i32);
        f64::from(natural)
    }

    /// Shows the dashboard, results or "No matches" for the current query.
    fn refresh(self: &Rc<Self>) {
        {
            let card = self.card.borrow();
            let Some(card) = card.as_ref() else { return };
            let query = card.query.text();
            let resting = query.is_empty();
            card.placeholder.set_visible(resting);
            card.dashboard.set_visible(resting);

            let entries = if resting { Vec::new() } else { results::search(&self.apps.borrow(), &query) };
            let count = entries.len();
            card.counter.set_text(&if resting { String::new() } else { format!("{count} / {}", self.apps.borrow().len()) });
            card.list.set_entries(entries);
            card.list.widget.set_visible(!resting && count > 0);
            card.list.widget.set_size_request(-1, ResultList::height(card.s, count).round() as i32);
            card.empty.set_visible(!resting && count == 0);
        }

        let launcher = Rc::downgrade(self);
        let target = self.natural_height();
        anim::animate_to(&self.stage, &self.height, target, move |_, _| {
            if let Some(launcher) = launcher.upgrade() {
                launcher.place();
            }
        });
    }

    fn launch(self: &Rc<Self>, app: gio::AppInfo) {
        self.hide();
        let context = WidgetExt::display(&self.window).app_launch_context();
        if let Err(err) = app.launch(&[], Some(&context)) {
            eprintln!("eclipse-shell: launching {} failed: {err}", app.display_name());
        }
    }

    fn connect_input(self: &Rc<Self>) {
        // Capture phase, so navigation keys never reach the text field.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let launcher = Rc::downgrade(self);
            move |_, key, _, _| {
                let Some(launcher) = launcher.upgrade() else { return glib::Propagation::Proceed };
                let list = launcher.card.borrow().as_ref().map(|c| c.list.clone());
                let Some(list) = list else { return glib::Propagation::Proceed };
                match key {
                    gdk::Key::Escape => launcher.hide(),
                    gdk::Key::Down | gdk::Key::Tab => list.step(1),
                    gdk::Key::Up | gdk::Key::ISO_Left_Tab => list.step(-1),
                    gdk::Key::Return | gdk::Key::KP_Enter => list.activate_selected(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        });
        self.window.add_controller(keys);

        // Clicks that miss the card close the launcher.
        let click = gtk::GestureClick::new();
        click.connect_pressed({
            let launcher = Rc::downgrade(self);
            move |_, _, x, y| {
                let Some(launcher) = launcher.upgrade() else { return };
                let card = launcher.card.borrow();
                let Some(card) = card.as_ref() else { return };
                let (left, top) = launcher.origin.get();
                let bottom = top + launcher.height.borrow().value.max(launcher.natural_height());
                let inside = (left..left + card.width).contains(&x) && (top..bottom).contains(&y);
                if !inside {
                    launcher.hide();
                }
            }
        });
        self.window.add_controller(click);
    }

    fn build_card(self: &Rc<Self>, s: f64) {
        if let Some(old) = self.card.take() {
            self.stage.remove(&old.background);
            self.stage.remove(&old.column);
        }
        let px = |v: f64| (v * s).round() as i32;
        let width = (560.0 * s).round();

        let background = gtk::DrawingArea::new();
        background.set_draw_func(|_, cr, w, h| {
            let palette = theme::palette();
            theme::squircle(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, config::LAUNCHER_RADIUS);
            theme::set_source(cr, palette.base);
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, palette.outline);
            let _ = cr.stroke();
        });

        // Search row: mark, query, result counter, help and all-apps buttons.
        let query = gtk::Text::builder().hexpand(true).valign(gtk::Align::Center).build();
        query.set_attributes(Some(&TextStyle::new(14.0 * s, pango::Weight::Normal, CREAM).attrs()));
        let placeholder = TextStyle::new(14.0 * s, pango::Weight::Normal, FAINT).label("Search apps, type / for commands");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_can_target(false);
        let field = gtk::Overlay::builder().hexpand(true).margin_start(px(12.0)).margin_end(px(10.0)).build();
        field.set_child(Some(&query));
        field.add_overlay(&placeholder);

        let counter = TextStyle::new(10.0 * s, pango::Weight::Normal, FAINT).label("");
        counter.set_margin_end(px(10.0));

        let glyph = mark::mark(19.0 * s);
        glyph.set_margin_start(px(16.0));
        let search_row = gtk::Box::builder().height_request(px(46.0)).margin_top(px(8.0)).build();
        search_row.append(&glyph);
        search_row.append(&field);
        search_row.append(&counter);
        search_row.append(&help_button(s));
        let grid = grid_button(s);
        grid.set_margin_start(px(4.0));
        grid.set_margin_end(px(10.0));
        search_row.append(&grid);

        let divider = gtk::DrawingArea::builder().height_request(1).margin_top(px(6.0)).margin_start(px(16.0)).margin_end(px(16.0)).build();
        divider.set_draw_func(|_, cr, w, h| {
            cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
            theme::set_source(cr, HAIR);
            let _ = cr.fill();
        });

        let dashboard = dashboard::dashboard(s);
        dashboard.set_margin_top(px(10.0));
        dashboard.set_margin_bottom(px(8.0));
        dashboard.set_margin_start(px(16.0));
        dashboard.set_margin_end(px(16.0));

        let list = ResultList::new(s);
        list.widget.set_margin_top(px(6.0));
        list.widget.set_margin_bottom(px(12.0));
        list.widget.set_margin_start(px(6.0));
        list.widget.set_margin_end(px(6.0));
        list.connect_launch({
            let launcher = Rc::downgrade(self);
            move |app| {
                if let Some(launcher) = launcher.upgrade() {
                    launcher.launch(app);
                }
            }
        });

        let empty = TextStyle::new(12.0 * s, pango::Weight::Normal, FAINT).label("No matches");
        empty.set_height_request(px(40.0));
        empty.set_valign(gtk::Align::Start);
        empty.set_margin_top(px(10.0));
        empty.set_margin_bottom(px(8.0));

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.set_size_request(width as i32, -1);
        for part in [search_row.upcast_ref::<gtk::Widget>(), divider.upcast_ref(), dashboard.upcast_ref(), list.widget.upcast_ref(), empty.upcast_ref()] {
            column.append(part);
        }

        query.connect_changed({
            let launcher = Rc::downgrade(self);
            move |_| {
                if let Some(launcher) = launcher.upgrade() {
                    launcher.refresh();
                }
            }
        });

        self.stage.put(&background, 0.0, 0.0);
        self.stage.put(&column, 0.0, 0.0);
        theme::on_palette_change({
            let background = background.downgrade();
            move || {
                if let Some(background) = background.upgrade() {
                    background.queue_draw();
                }
            }
        });
        *self.card.borrow_mut() = Some(Card { s, width, background, column, query, placeholder, counter, dashboard, list, empty });
    }
}

/// Card height with an empty query: search row, divider and dashboard.
fn resting_height(s: f64) -> f64 {
    46.0 * s + 1.0 + dashboard::height(s) + 32.0 * s
}


/// A 30px square button that tints on hover, drawn with `paint`.
fn square_button(s: f64, paint: impl Fn(&gtk::cairo::Context, f64, f64, bool) + 'static) -> gtk::DrawingArea {
    let button = gtk::DrawingArea::builder().valign(gtk::Align::Center).build();
    let size = (30.0 * s).round() as i32;
    button.set_size_request(size, size);
    button.set_cursor_from_name(Some("pointer"));
    let hovered = Rc::new(Cell::new(false));
    button.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            if hovered.get() {
                theme::rounded_rect(cr, 0.0, 0.0, w, h, 9.0 * s);
                theme::set_source(cr, CHIP_FILL);
                let _ = cr.fill();
            }
            paint(cr, w, h, hovered.get());
        }
    });
    let motion = gtk::EventControllerMotion::new();
    for entering in [true, false] {
        let (hovered, button) = (hovered.clone(), button.downgrade());
        let set = move || {
            hovered.set(entering);
            if let Some(button) = button.upgrade() {
                button.queue_draw();
            }
        };
        if entering {
            motion.connect_enter(move |_, _, _| set());
        } else {
            motion.connect_leave(move |_| set());
        }
    }
    button.add_controller(motion);
    button
}

fn icon_tint(hovered: bool) -> theme::Rgba {
    if hovered { theme::palette().accent_lit } else { ICON_DIM }
}

/// "?" opens the cheat sheet (not built yet).
fn help_button(s: f64) -> gtk::DrawingArea {
    square_button(s, move |cr, w, h, hovered| {
        let layout = pangocairo_layout(cr, "?", TextStyle::new(15.0 * s, pango::Weight::Semibold, icon_tint(hovered)));
        let (tw, th) = layout.pixel_size();
        cr.move_to((w - f64::from(tw)) / 2.0, (h - f64::from(th)) / 2.0);
        theme::set_source(cr, icon_tint(hovered));
        pangocairo::functions::show_layout(cr, &layout);
    })
}

/// A 3×3 grid of dots; opens the all-apps grid (not built yet).
fn grid_button(s: f64) -> gtk::DrawingArea {
    square_button(s, move |cr, w, h, hovered| {
        let (dot, gap) = (3.5 * s, 3.0 * s);
        let span = dot * 3.0 + gap * 2.0;
        let (x0, y0) = ((w - span) / 2.0, (h - span) / 2.0);
        for row in 0..3 {
            for col in 0..3 {
                theme::rounded_rect(cr, x0 + f64::from(col) * (dot + gap), y0 + f64::from(row) * (dot + gap), dot, dot, 1.0 * s);
            }
        }
        theme::set_source(cr, icon_tint(hovered));
        let _ = cr.fill();
    })
}

fn pangocairo_layout(cr: &gtk::cairo::Context, text: &str, style: TextStyle) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text(text);
    layout.set_font_description(Some(&style.font()));
    layout
}
