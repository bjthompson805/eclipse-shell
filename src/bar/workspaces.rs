//! Workspace strip: a free-standing pill per workspace. Empty ones are faint,
//! occupied ones brighter, and the active one turns accent-coloured: wide and
//! numbered when empty, or carrying up to three app icons when occupied.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{glib, pango};
use gtk4 as gtk;

use crate::anim::{self, Easing, Tween};
use crate::config;
use crate::hyprland::{self, State};
use crate::icons;
use crate::theme::{self, Rgba, TextStyle, CREAM};

const EFFECTS_MS: f64 = 200.0;
const MAX_ICONS: usize = 3;

#[derive(Clone, Copy)]
struct Metrics {
    dot: f64,
    active_len: f64,
    gap: f64,
    icon: f64,
    icon_gap: f64,
    icon_pad_along: f64,
    /// Pill thickness, shared so pills line up with or without icons.
    cross: f64,
    number_px: f64,
}

impl Metrics {
    fn new(s: f64) -> Self {
        let dot = 10.0 * s;
        let icon = 9.0 * s;
        let icon_pad_cross = 4.0 * s;
        let cross = if config::WORKSPACE_ICONS { dot.max(icon + 2.0 * icon_pad_cross) } else { dot };
        Self {
            dot,
            active_len: dot * 2.2,
            gap: 4.0 * s,
            icon,
            icon_gap: 3.0 * s,
            icon_pad_along: 5.0 * s,
            cross,
            number_px: 8.5 * s,
        }
    }

    fn length(&self, icons: usize, active: bool) -> f64 {
        if icons > 0 {
            icons as f64 * self.icon + (icons - 1) as f64 * self.icon_gap + 2.0 * self.icon_pad_along
        } else if active {
            self.active_len
        } else {
            self.dot
        }
    }
}

struct Pill {
    widget: gtk::Overlay,
    shape: gtk::DrawingArea,
    icons: gtk::Box,
    number: gtk::Label,
    classes: RefCell<Vec<String>>,
    occupied: std::cell::Cell<bool>,
    /// 0 = inactive, 1 = active; drives colour, icon opacity and the number.
    active: Rc<RefCell<Tween>>,
    length: Rc<RefCell<Tween>>,
}

impl Pill {
    fn new(id: i32, m: Metrics) -> Rc<Self> {
        let shape = gtk::DrawingArea::new();
        let icons = gtk::Box::builder().spacing(m.icon_gap.round() as i32).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        // Numbered within its group of ten, like the keybindings.
        let number = TextStyle::new(m.number_px, pango::Weight::Bold, theme::palette().recessed).label(&((id - 1).rem_euclid(10) + 1).to_string());
        number.set_halign(gtk::Align::Center);
        number.set_valign(gtk::Align::Center);

        let widget = gtk::Overlay::builder().valign(gtk::Align::Center).build();
        widget.set_child(Some(&shape));
        widget.add_overlay(&icons);
        widget.add_overlay(&number);
        widget.set_cursor_from_name(Some("pointer"));

        let pill = Rc::new(Self {
            widget,
            shape,
            icons,
            number,
            classes: RefCell::default(),
            occupied: Default::default(),
            active: Tween::shared(EFFECTS_MS, Easing::OutCubic),
            length: Tween::shared(EFFECTS_MS, Easing::OutCubic),
        });

        pill.shape.set_draw_func({
            let pill = Rc::downgrade(&pill);
            move |_, cr, w, h| {
                let Some(pill) = pill.upgrade() else { return };
                let (w, h) = (f64::from(w), f64::from(h));
                theme::rounded_rect(cr, 0.0, 0.0, w, h, w.min(h) / 2.0);
                theme::set_source(cr, pill.colour());
                let _ = cr.fill();
            }
        });

        let click = gtk::GestureClick::new();
        click.connect_released(move |_, _, _, _| hyprland::focus_workspace(id));
        pill.widget.add_controller(click);
        pill
    }

    fn colour(&self) -> Rgba {
        let rest = if self.occupied.get() { CREAM.alpha(0.55) } else { CREAM.alpha(0.18) };
        let accent = theme::palette().accent;
        let t = self.active.borrow().value;
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Rgba { r: mix(rest.r, accent.r), g: mix(rest.g, accent.g), b: mix(rest.b, accent.b), a: mix(rest.a, accent.a) }
    }

    fn update(self: &Rc<Self>, m: Metrics, active: bool, occupied: bool, classes: Vec<String>) {
        self.occupied.set(occupied);
        if *self.classes.borrow() != classes {
            while let Some(child) = self.icons.first_child() {
                self.icons.remove(&child);
            }
            for class in &classes {
                let image = gtk::Image::from_gicon(&icons::for_class(class));
                image.set_pixel_size(m.icon.round() as i32);
                self.icons.append(&image);
            }
            *self.classes.borrow_mut() = classes;
        }
        let has_icons = !self.classes.borrow().is_empty();

        let pill = self.clone();
        anim::animate_to(&self.shape, &self.active, if active { 1.0 } else { 0.0 }, move |shape, t| {
            pill.icons.set_opacity(0.55 + 0.45 * t);
            pill.number.set_opacity(if has_icons { 0.0 } else { t });
            shape.queue_draw();
        });
        let cross = m.cross.round() as i32;
        anim::animate_to(&self.shape, &self.length, m.length(self.classes.borrow().len(), active), move |shape, len| {
            shape.set_size_request(len.round() as i32, cross);
        });
    }
}

pub struct Workspaces {
    pub widget: gtk::Box,
    metrics: Metrics,
    pills: RefCell<HashMap<i32, Rc<Pill>>>,
    shown: RefCell<Vec<i32>>,
    /// What the pills last showed: (id, active, app classes) each.
    last_view: RefCell<Option<Vec<(i32, bool, Vec<String>)>>>,
}

impl Workspaces {
    pub fn new(s: f64) -> Rc<Self> {
        let metrics = Metrics::new(s);
        let widget = gtk::Box::builder().spacing(metrics.gap.round() as i32).valign(gtk::Align::Center).build();
        let strip = Rc::new(Self { widget, metrics, pills: RefCell::default(), shown: RefCell::default(), last_view: RefCell::default() });

        // Scrolling walks to the neighbouring workspace.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
        scroll.connect_scroll({
            let strip = Rc::downgrade(&strip);
            move |_, _, dy| {
                if let Some(strip) = strip.upgrade() {
                    strip.walk(if dy < 0.0 { -1 } else { 1 });
                }
                glib::Propagation::Stop
            }
        });
        strip.widget.add_controller(scroll);

        theme::on_palette_change({
            let strip = Rc::downgrade(&strip);
            move || {
                let Some(strip) = strip.upgrade() else { return };
                for pill in strip.pills.borrow().values() {
                    TextStyle::new(strip.metrics.number_px, pango::Weight::Bold, theme::palette().recessed).apply(&pill.number);
                    pill.shape.queue_draw();
                }
            }
        });
        strip
    }

    fn active_id(&self) -> Option<i32> {
        self.pills.borrow().iter().find(|(_, p)| p.active.borrow().target() == 1.0).map(|(id, _)| *id)
    }

    fn walk(&self, step: i32) {
        let shown = self.shown.borrow();
        let Some(index) = self.active_id().and_then(|id| shown.iter().position(|&w| w == id)) else { return };
        if let Some(&id) = shown.get((index as i32 + step).max(0) as usize) {
            hyprland::focus_workspace(id);
        }
    }

    pub fn update(&self, state: &State, connector: Option<&str>) {
        let active = connector.and_then(|c| state.active.get(c)).copied().unwrap_or(1);

        let mut classes: HashMap<i32, Vec<String>> = HashMap::new();
        for window in &state.windows {
            let list = classes.entry(window.workspace).or_default();
            if config::WORKSPACE_ICONS && !window.class.is_empty() && !list.contains(&window.class) && list.len() < MAX_ICONS {
                list.push(window.class.clone());
            }
        }

        let ids = shown_workspaces(active, |id| classes.contains_key(&id));
        // Title changes arrive here too; leave the pills alone unless what
        // they show has changed.
        let view: Vec<(i32, bool, Vec<String>)> = ids.iter().map(|&id| (id, id == active, classes.get(&id).cloned().unwrap_or_default())).collect();
        if self.last_view.borrow().as_ref() == Some(&view) {
            return;
        }
        *self.last_view.borrow_mut() = Some(view);
        let mut pills = self.pills.borrow_mut();
        pills.retain(|id, _| ids.contains(id));
        while let Some(child) = self.widget.first_child() {
            self.widget.remove(&child);
        }
        for &id in &ids {
            let pill = pills.entry(id).or_insert_with(|| Pill::new(id, self.metrics)).clone();
            pill.update(self.metrics, id == active, classes.contains_key(&id), classes.get(&id).cloned().unwrap_or_default());
            self.widget.append(&pill.widget);
        }
        *self.shown.borrow_mut() = ids;
    }
}

/// Workspaces 1..=10 of the active group of ten. Occupied-only shows every
/// workspace up to the highest occupied one plus the active one; otherwise a
/// run of at least five that grows to cover any used higher number.
fn shown_workspaces(active: i32, occupied: impl Fn(i32) -> bool) -> Vec<i32> {
    let base = (active - 1).div_euclid(10) * 10;
    if config::OCCUPIED_WORKSPACES_ONLY {
        let highest = (1..=10).filter(|&i| occupied(base + i)).max().unwrap_or(0);
        let ids: Vec<i32> = (1..=10).map(|i| base + i).filter(|&id| id - base <= highest || id == active).collect();
        if ids.is_empty() { vec![active] } else { ids }
    } else {
        let last = (6..=10).rev().find(|&i| occupied(base + i) || base + i == active).unwrap_or(5);
        (1..=last).map(|i| base + i).collect()
    }
}
