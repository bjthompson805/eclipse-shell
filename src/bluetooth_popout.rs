//! Bluetooth popout, dropping from the Bluetooth glyph in the status cluster:
//! the adapter switch, a rescan button, and the devices, connected first. Clicking a
//! device disconnects it if connected, connects it if paired, and otherwise
//! pairs it; a paired one shows a forget button on hover.
//!
//! Readings come from src/bluetooth.rs every couple of seconds while the card
//! is open (every second during a scan, as names trickle in). Every change is
//! a few seconds of SDP and eclipse-priv, so one runs at a time.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::bluetooth::{self, Device, Status};
use crate::hint;
use crate::popout::{self, on_click, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, FAINT, FLAME, ICON_DIM, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-bluetooth";
const TICK: Duration = Duration::from_secs(1);
/// Ticks between readings while no scan runs.
const IDLE_TICKS: u32 = 2;
const WIDTH: f64 = 300.0;

pub struct BluetoothPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
    status: RefCell<Rc<Status>>,
    shape: RefCell<String>,
    scanning: Cell<bool>,
    /// The device a change is running for.
    busy: RefCell<Option<String>>,
    /// The last change that failed: the device (none for the adapter) and why.
    failure: RefCell<Option<(Option<String>, String)>>,
}

impl BluetoothPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            generation: Cell::new(0),
            status: RefCell::default(),
            shape: RefCell::default(),
            scanning: Cell::new(false),
            busy: RefCell::default(),
            failure: RefCell::default(),
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
        *self.failure.borrow_mut() = None;
        self.shape.borrow_mut().clear();
        let bt = self.clone();
        glib::spawn_future_local(async move {
            bt.refresh().await;
            bt.popout.show(Anchor::Under(x));
        });

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ticks = Cell::new(0);
        let bt = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match bt.upgrade() {
            Some(b) if b.popout.is_open() && b.generation.get() == generation => {
                ticks.set(ticks.get() + 1);
                if b.scanning.get() || ticks.get() % IDLE_TICKS == 0 {
                    glib::spawn_future_local(async move { b.refresh().await });
                }
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    async fn refresh(self: &Rc<Self>) {
        let Ok(status) = gio::spawn_blocking(bluetooth::read).await else { return };
        *self.status.borrow_mut() = Rc::new(status);
        if *self.shape.borrow() != self.shape() {
            self.rebuild();
        }
    }

    /// Everything the card shows; there's nothing to update in place.
    fn shape(&self) -> String {
        let status = self.status.borrow();
        let mut shape = format!(
            "{} {} {} {} {:?} {:?}|",
            status.present,
            status.enabled,
            status.can_change,
            self.scanning.get(),
            self.busy.borrow(),
            self.failure.borrow()
        );
        for d in &status.devices {
            shape += &format!("{} {} {} {} {}|", d.address, d.name, d.paired, d.connected, d.glyph);
        }
        shape
    }

    fn rebuild(self: &Rc<Self>) {
        *self.shape.borrow_mut() = self.shape();
        let s = self.popout.scale();
        let status = self.status.borrow().clone();
        self.popout.set_content(&popout::card(&self.body(s, &status)));
    }

    // ---- actions ---------------------------------------------------------

    fn scan(self: &Rc<Self>) {
        if self.scanning.replace(true) {
            return;
        }
        self.rebuild();
        let bt = self.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(bluetooth::scan).await;
            bt.scanning.set(false);
            if let Ok(Err(why)) = result {
                *bt.failure.borrow_mut() = Some((None, why));
            }
            bt.refresh().await;
        });
    }

    /// Runs one change for `device` (none for the adapter) off the main
    /// thread, then reads everything again.
    fn change(self: &Rc<Self>, device: Option<String>, work: impl FnOnce() -> Result<(), String> + Send + 'static) {
        if self.busy.borrow().is_some() {
            return;
        }
        *self.busy.borrow_mut() = Some(device.clone().unwrap_or_default());
        *self.failure.borrow_mut() = None;
        self.rebuild();
        let bt = self.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(work).await.unwrap_or_else(|_| Err("It stopped unexpectedly.".into()));
            *bt.busy.borrow_mut() = None;
            if let Err(why) = result {
                *bt.failure.borrow_mut() = Some((device, why));
            }
            bt.refresh().await;
        });
    }

    fn pick(self: &Rc<Self>, device: &Device) {
        let address = device.address.clone();
        let target = address.clone();
        match (device.connected, device.paired) {
            (true, _) => self.change(Some(address), move || bluetooth::disconnect(&target)),
            (false, true) => self.change(Some(address), move || bluetooth::connect(&target)),
            (false, false) => self.change(Some(address), move || bluetooth::pair(&target)),
        }
    }

    // ---- the card --------------------------------------------------------

    fn body(self: &Rc<Self>, s: f64, status: &Status) -> gtk::Box {
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

        let head = gtk::Box::builder().spacing(px(8.0)).height_request(px(24.0)).build();
        head.append(&theme::filled_icon_glyph("bluetooth", 16.0 * s, theme::palette().accent));
        let title = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("BLUETOOTH");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        head.append(&title);
        if status.enabled {
            head.append(&self.scan_button(s));
        }
        if status.present {
            let bt = Rc::downgrade(self);
            let on = status.enabled;
            let switch = popout::switch(s, on, status.can_change, move || {
                if let Some(b) = bt.upgrade() {
                    b.change(None, move || bluetooth::set_power(!on));
                }
            });
            switch.set_margin_start(px(4.0));
            head.append(&switch);
        }
        column.append(&head);
        column.append(&popout::divider());

        if let Some((None, why)) = &*self.failure.borrow() {
            column.append(&failure(s, why));
        }
        if !status.can_change {
            column.append(&note(s, "Run install.sh to pair and connect devices."));
        }
        match () {
            _ if !status.present => column.append(&note(s, "No Bluetooth adapter")),
            _ if !status.enabled => column.append(&note(s, "Bluetooth is off")),
            _ if status.devices.is_empty() && self.scanning.get() => column.append(&note(s, "Looking for devices…")),
            _ if status.devices.is_empty() => column.append(&note(s, "No devices yet. Scan to find some in pairing mode.")),
            _ => {
                let list = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
                for device in &status.devices {
                    list.append(&self.device_row(s, device, status.can_change));
                    if let Some((Some(address), why)) = &*self.failure.borrow()
                        && address == &device.address
                    {
                        let label = failure(s, why);
                        label.set_margin_start(px(42.0));
                        list.append(&label);
                    }
                }
                let scroller = gtk::ScrolledWindow::builder()
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .propagate_natural_height(true)
                    .max_content_height(px(240.0))
                    .child(&list)
                    .build();
                column.append(&scroller);
            }
        }
        column
    }

    /// The rescan glyph: dim, cream on hover, flame while an inquiry runs.
    fn scan_button(self: &Rc<Self>, s: f64) -> gtk::Label {
        if self.scanning.get() {
            return theme::icon_glyph("refresh", 16.0 * s, FLAME);
        }
        let button = theme::icon_glyph("refresh", 16.0 * s, ICON_DIM);
        on_hover(&button, {
            let button = button.downgrade();
            move |inside| {
                if let Some(b) = button.upgrade() {
                    b.set_attributes(theme::icon_glyph("refresh", 16.0 * s, if inside { CREAM } else { ICON_DIM }).attributes().as_ref());
                }
            }
        });
        let bt = Rc::downgrade(self);
        on_click(&button, move || bt.upgrade().into_iter().for_each(|b| b.scan()));
        button
    }

    fn device_row(self: &Rc<Self>, s: f64, device: &Device, can_change: bool) -> gtk::Overlay {
        let px = |v: f64| (v * s).round() as i32;
        let palette = theme::palette();
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

        let content = gtk::Box::builder().spacing(px(10.0)).height_request(px(38.0)).margin_start(px(6.0)).margin_end(px(8.0)).build();
        content.append(&tile(s, device.glyph, if device.connected { palette.accent_lit } else { ICON_DIM }));

        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(px(1.0)).valign(gtk::Align::Center).hexpand(true).build();
        let (weight, color) = if device.connected { (pango::Weight::Semibold, CREAM) } else { (pango::Weight::Medium, SUBTLE) };
        let name = TextStyle::new(11.5 * s, weight, color).label(&device.name);
        popout::fill(&name);
        text.append(&name);
        let state = if device.connected { Some("connected") } else if device.paired { Some("paired") } else { None };
        let meta: Vec<&str> = state.into_iter().chain(device.kind.label()).collect();
        if !meta.is_empty() {
            let meta = TextStyle::new(9.5 * s, pango::Weight::Normal, FAINT).label(&meta.join(" · "));
            popout::fill(&meta);
            text.append(&meta);
        }
        content.append(&text);

        let busy = self.busy.borrow().as_deref() == Some(&device.address);
        let mut forget = None;
        if busy {
            content.append(&TextStyle::new(9.5 * s, pango::Weight::Semibold, FLAME).label(if device.connected { "Leaving…" } else { "Joining…" }));
        } else if !device.paired {
            content.append(&chip(s, "Pair"));
        } else if can_change {
            // Forget, shown while the row is hovered.
            let button = theme::icon_glyph("close", 14.0 * s, ICON_DIM);
            button.set_opacity(0.0);
            // Claiming the press keeps the row's own click from firing too.
            let click = gtk::GestureClick::new();
            click.connect_pressed(|gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });
            let (bt, address) = (Rc::downgrade(self), device.address.clone());
            click.connect_released(move |_, _, _, _| {
                if let Some(b) = bt.upgrade() {
                    let target = address.clone();
                    b.change(Some(address.clone()), move || bluetooth::forget(&target));
                }
            });
            button.add_controller(click);
            button.set_cursor_from_name(Some("pointer"));
            // Says what it does the moment it's hovered, unlike a tooltip.
            let hint = hint::pill(s, "Forget");
            hint.set_visible(false);
            on_hover(&button, {
                let (button, hint) = (button.downgrade(), hint.downgrade());
                move |inside| {
                    if let Some(b) = button.upgrade() {
                        b.set_attributes(theme::icon_glyph("close", 14.0 * s, if inside { CREAM } else { ICON_DIM }).attributes().as_ref());
                    }
                    if let Some(h) = hint.upgrade() {
                        h.set_visible(inside);
                    }
                }
            });
            content.append(&hint);
            content.append(&button);
            forget = Some(button);
        }

        let row = gtk::Overlay::new();
        row.set_child(Some(&background));
        row.add_overlay(&content);
        row.set_measure_overlay(&content, true);
        on_hover(&row, move |inside| {
            hovered.set(inside);
            background.queue_draw();
            if let Some(f) = &forget {
                f.set_opacity(if inside { 1.0 } else { 0.0 });
            }
        });
        if can_change {
            let (bt, device) = (Rc::downgrade(self), device.clone());
            on_click(&row, move || bt.upgrade().into_iter().for_each(|b| b.pick(&device)));
        }
        row
    }
}

/// A device's glyph on a small rounded tile.
fn tile(s: f64, glyph: &str, color: theme::Rgba) -> gtk::Overlay {
    let size = (26.0 * s).round() as i32;
    let back = gtk::DrawingArea::builder().content_width(size).content_height(size).build();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 7.0 * s);
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
    });
    let tile = gtk::Overlay::builder().valign(gtk::Align::Center).build();
    tile.set_child(Some(&back));
    tile.add_overlay(&theme::filled_icon_glyph(glyph, 15.0 * s, color));
    tile
}

/// A small outlined pill with a word in it.
fn chip(s: f64, text: &str) -> gtk::Overlay {
    let label = TextStyle::new(9.5 * s, pango::Weight::Semibold, theme::DIM).label(text);
    label.set_margin_start((8.0 * s).round() as i32);
    label.set_margin_end((8.0 * s).round() as i32);
    let back = gtk::DrawingArea::new();
    back.set_draw_func(|_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
    });
    let chip = gtk::Overlay::builder().valign(gtk::Align::Center).height_request((18.0 * s).round() as i32).build();
    chip.set_child(Some(&back));
    chip.add_overlay(&label);
    chip.set_measure_overlay(&label, true);
    chip
}

fn note(s: f64, text: &str) -> gtk::Label {
    let label = TextStyle::new(10.5 * s, pango::Weight::Normal, FAINT).label(text);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}

fn failure(s: f64, text: &str) -> gtk::Label {
    let label = TextStyle::new(9.5 * s, pango::Weight::Medium, theme::palette().accent_lit).label(text);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}
