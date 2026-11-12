//! Toasts: new notifications as cards under the bar's right end, newest on
//! top, three at most with a count of the rest. Each leaves after its time
//! (the app's, or a few seconds) unless the pointer rests on it; critical
//! ones stay until dealt with. Clicking one runs its default action and
//! brings its app forward. Do Not Disturb holds back all but critical ones;
//! everything still reaches the inbox.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::notifications::{self, Change, Notification, Picture, Urgency};
use crate::popout::{on_click, on_hover};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};
use crate::{config, popout};

const SHOWN: usize = 3;
const WIDTH: f64 = 360.0;
const NORMAL_MS: u32 = 6000;
const LOW_MS: u32 = 4000;
const FADE: Duration = Duration::from_millis(180);

pub struct Toasts {
    window: gtk::ApplicationWindow,
    column: gtk::Box,
    /// Toasts waiting or showing, newest first.
    active: RefCell<Vec<u32>>,
    timers: RefCell<HashMap<u32, glib::SourceId>>,
    /// The toast under the pointer; its time doesn't run.
    hovered: RefCell<Option<u32>>,
    monitor: RefCell<Option<gdk::Monitor>>,
}

impl Toasts {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).css_classes(["eclipse-shell"]).build();
        window.init_layer_shell();
        window.set_namespace(Some("eclipse-shell-toasts"));
        window.set_layer(Layer::Top);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);
        // Below the bar's reserved band, not over it.
        window.set_exclusive_zone(0);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&column));

        let toasts = Rc::new(Self { window, column, active: RefCell::default(), timers: RefCell::default(), hovered: RefCell::default(), monitor: RefCell::default() });
        let weak = Rc::downgrade(&toasts);
        notifications::on_change(move |change| {
            if let Some(t) = weak.upgrade() {
                t.changed(change);
            }
        });
        toasts
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        if self.active.borrow().is_empty() {
            *self.monitor.borrow_mut() = monitor;
        }
    }

    fn scale(&self) -> f64 {
        config::bar_scale(self.monitor.borrow().as_ref().map_or(1080, |m| m.geometry().height()))
    }

    fn changed(self: &Rc<Self>, change: Change) {
        match change {
            Change::Added(id) => {
                let Some(n) = notifications::get(id) else { return };
                if notifications::dnd() && n.urgency != Urgency::Critical {
                    return;
                }
                self.active.borrow_mut().insert(0, id);
            }
            // An update shows in place; one whose toast is gone doesn't pop up again.
            Change::Replaced(id) if self.active.borrow().contains(&id) => {}
            Change::Removed(id) => {
                self.active.borrow_mut().retain(|a| *a != id);
                if let Some(timer) = self.timers.borrow_mut().remove(&id) {
                    timer.remove();
                }
            }
            _ => return,
        }
        self.render(match change {
            Change::Added(id) => Some(id),
            _ => None,
        });
    }

    /// Lays out the newest toasts; `fresh` fades in.
    fn render(self: &Rc<Self>, fresh: Option<u32>) {
        while let Some(child) = self.column.first_child() {
            self.column.remove(&child);
        }
        let active = self.active.borrow().clone();
        if active.is_empty() {
            self.window.set_visible(false);
            return;
        }
        let s = self.scale();
        let px = |v: f64| (v * s).round() as i32;
        self.window.set_monitor(self.monitor.borrow().as_ref());
        self.window.set_margin(Edge::Top, px(6.0));
        self.window.set_margin(Edge::Right, (config::get().frame_edge + 8.0 * s).round() as i32);
        self.column.set_spacing(px(8.0));

        for id in active.iter().take(SHOWN) {
            let Some(n) = notifications::get(*id) else { continue };
            let card = self.card(s, &n);
            if fresh == Some(*id) {
                fade_in(&card);
            }
            self.column.append(&card);
            self.start_timer(&n);
        }
        if active.len() > SHOWN {
            let more = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).label(&format!("+{} more in the inbox", active.len() - SHOWN));
            let pill = crate::hint::framed(s, &more, 10.0, 4.0);
            pill.set_halign(gtk::Align::End);
            self.column.append(&pill);
        }
        self.window.present();
    }

    /// Starts a shown toast's clock, unless it's running, paused under the
    /// pointer, or the toast never expires.
    fn start_timer(self: &Rc<Self>, n: &Notification) {
        let ms = match n.timeout {
            _ if n.urgency == Urgency::Critical => return,
            Some(0) => return,
            Some(ms) => ms,
            None if n.urgency == Urgency::Low => LOW_MS,
            None => NORMAL_MS,
        };
        let id = n.id;
        if self.timers.borrow().contains_key(&id) || *self.hovered.borrow() == Some(id) {
            return;
        }
        let toasts = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(Duration::from_millis(u64::from(ms)), move || {
            let Some(t) = toasts.upgrade() else { return };
            t.timers.borrow_mut().remove(&id);
            t.active.borrow_mut().retain(|a| *a != id);
            notifications::expired(id);
            t.render(None);
        });
        self.timers.borrow_mut().insert(id, timer);
    }

    fn card(self: &Rc<Self>, s: f64, n: &Notification) -> gtk::Overlay {
        let px = |v: f64| (v * s).round() as i32;
        let critical = n.urgency == Urgency::Critical;
        let back = gtk::DrawingArea::new();
        back.set_draw_func(move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 12.0 * s);
            theme::set_source(cr, palette.base);
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, if critical { palette.accent_lit } else { palette.outline });
            let _ = cr.stroke();
            if critical {
                theme::rounded_rect(cr, 0.0, 10.0 * s, 3.0 * s, h - 20.0 * s, 1.5 * s);
                theme::set_source(cr, palette.accent_lit);
                let _ = cr.fill();
            }
        });

        let content = gtk::Box::builder().spacing(px(12.0)).width_request(px(WIDTH)).margin_top(px(12.0)).margin_bottom(px(12.0)).margin_start(px(14.0)).margin_end(px(12.0)).build();
        let icon = picture(s, n.picture.as_ref(), 36.0);
        icon.set_valign(gtk::Align::Start);
        content.append(&icon);

        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(px(3.0)).hexpand(true).build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
        let app = TextStyle::new(9.5 * s, pango::Weight::Semibold, DIM).mono().tracking(1.0 * s).label(&n.app.to_uppercase());
        popout::fill(&app);
        head.append(&app);
        head.append(&TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).label(&age(&n.arrived)));
        let close = theme::icon_glyph("close", 14.0 * s, ICON_DIM);
        close.set_opacity(0.0);
        claim_click(&close, {
            let id = n.id;
            move || notifications::dismiss(id)
        });
        head.append(&close);
        text.append(&head);

        if !n.summary.is_empty() {
            let summary = TextStyle::new(12.0 * s, pango::Weight::Semibold, CREAM).label(&n.summary);
            wrapped(&summary, 2);
            text.append(&summary);
        }
        if !n.body.is_empty() {
            let body = TextStyle::new(11.0 * s, pango::Weight::Normal, SUBTLE).label(&n.body);
            wrapped(&body, 4);
            text.append(&body);
        }
        let buttons: Vec<&(String, String)> = n.buttons().collect();
        if !buttons.is_empty() {
            let row = gtk::Box::builder().spacing(px(6.0)).margin_top(px(6.0)).build();
            for (key, label) in buttons {
                let (id, key) = (n.id, key.clone());
                row.append(&action_button(s, label, move || notifications::invoke(id, &key)));
            }
            text.append(&row);
        }
        content.append(&text);

        let card = gtk::Overlay::new();
        card.set_child(Some(&back));
        card.add_overlay(&content);
        card.set_measure_overlay(&content, true);
        card.set_cursor_from_name(Some("pointer"));
        let id = n.id;
        on_click(&card, move || notifications::activate(id));
        let toasts = Rc::downgrade(self);
        let close = close.downgrade();
        on_hover(&card, move |inside| {
            close.upgrade().into_iter().for_each(|c| c.set_opacity(if inside { 1.0 } else { 0.0 }));
            let Some(t) = toasts.upgrade() else { return };
            if inside {
                // Hold its time while it's being read.
                *t.hovered.borrow_mut() = Some(id);
                if let Some(timer) = t.timers.borrow_mut().remove(&id) {
                    timer.remove();
                }
            } else {
                *t.hovered.borrow_mut() = None;
                if let Some(n) = notifications::get(id).filter(|_| t.active.borrow().iter().take(SHOWN).any(|a| *a == id)) {
                    t.start_timer(&n);
                }
            }
        });
        card
    }
}

/// The notification's own image or icon, or a bell on a tile.
pub fn picture(s: f64, picture: Option<&Picture>, size: f64) -> gtk::Widget {
    let px = (size * s).round() as i32;
    match picture {
        // gtk::Image draws a paintable at its pixel size; a gtk::Picture would
        // ask for the image's own size, which for a big icon file is huge.
        Some(Picture::Texture(texture)) => {
            let image = gtk::Image::from_paintable(Some(texture));
            image.set_pixel_size(px);
            image.upcast()
        }
        Some(Picture::Icon(icon)) => {
            let image = gtk::Image::from_gicon(icon);
            image.set_pixel_size(px);
            image.upcast()
        }
        None => {
            let glyph = theme::filled_icon_glyph("notifications", size * 0.55 * s, ICON_DIM);
            glyph.set_size_request(px, px);
            glyph.upcast()
        }
    }
}

/// How long ago: "now", "5m", "2h", or the weekday.
pub fn age(at: &glib::DateTime) -> String {
    let Ok(now) = glib::DateTime::now_local() else { return String::new() };
    let minutes = now.difference(at).as_minutes();
    match minutes {
        m if m < 1 => "now".into(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h", m / 60),
        _ => at.format("%a").map(String::from).unwrap_or_default(),
    }
}

fn wrapped(label: &gtk::Label, lines: i32) {
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label.set_lines(lines);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
}

/// A click that's this widget's alone, not also the card's beneath it.
pub fn claim_click(widget: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    widget.set_cursor_from_name(Some("pointer"));
    let click = gtk::GestureClick::new();
    click.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    click.connect_released(move |_, _, _, _| f());
    widget.add_controller(click);
}

/// An action as an outlined pill that fills on hover.
pub fn action_button(s: f64, label: &str, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let text = TextStyle::new(10.0 * s, pango::Weight::Semibold, CREAM).label(label);
    let (x, y) = ((10.0 * s).round() as i32, (4.0 * s).round() as i32);
    text.set_margin_start(x);
    text.set_margin_end(x);
    text.set_margin_top(y);
    text.set_margin_bottom(y);
    let hovered = Rc::new(std::cell::Cell::new(false));
    let back = gtk::DrawingArea::new();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
            theme::set_source(cr, if hovered.get() { palette.accent.alpha(0.3) } else { palette.raised });
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            theme::set_source(cr, if hovered.get() { palette.accent_lit } else { palette.outline });
            let _ = cr.stroke();
        }
    });
    let button = gtk::Overlay::new();
    button.set_child(Some(&back));
    button.add_overlay(&text);
    button.set_measure_overlay(&text, true);
    on_hover(&button, move |inside| {
        hovered.set(inside);
        back.queue_draw();
    });
    claim_click(&button, pressed);
    button
}

fn fade_in(widget: &impl IsA<gtk::Widget>) {
    widget.set_opacity(0.0);
    let start = std::cell::Cell::new(None::<i64>);
    widget.add_tick_callback(move |w, clock| {
        let now = clock.frame_time();
        let begun = start.get().unwrap_or(now);
        start.set(Some(begun));
        let t = (now - begun) as f64 / FADE.as_micros() as f64;
        w.set_opacity(t.clamp(0.0, 1.0));
        if t >= 1.0 { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
    });
}
