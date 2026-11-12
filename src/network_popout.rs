//! Network popout, dropping from the network glyph in the status cluster: the
//! wired link and the joined Wi-Fi network, then the Wi-Fi radio, a rescan,
//! and the networks in range. Clicking a network leaves it if joined, joins it
//! if known or open, and otherwise asks for its password in place.
//!
//! Readings come from src/wifi.rs every few seconds while the card is open,
//! with a fresh scan now and then. The card is only rebuilt when what it lists
//! changes; signal and addresses are updated in place, so a password being
//! typed survives. The radio goes through eclipse-priv (see system/).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::popout::{self, on_click, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, FAINT, FLAME, ICON_DIM, SUBTLE};
use crate::wifi::{self, Network, Status};

const NAMESPACE: &str = "eclipse-shell-network";
const HELPER: &str = "/usr/local/libexec/eclipse-priv";
const TICK: Duration = Duration::from_secs(3);
/// A connected supplicant doesn't rescan by itself, so rescan every this many ticks.
const SCAN_EVERY: u32 = 7;
/// How long the rescan button spins.
const SCAN_SPIN: Duration = Duration::from_secs(4);
const WIDTH: f64 = 300.0;

/// Widgets the readings update without a rebuild.
#[derive(Default)]
struct Live {
    wired: Option<gtk::Label>,
    joined: Option<gtk::Label>,
    signals: Vec<(String, gtk::Label)>,
}

pub struct NetworkPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
    status: RefCell<Rc<Status>>,
    shape: RefCell<String>,
    live: RefCell<Live>,
    /// The network whose password row is open, and what's typed so far.
    asking: RefCell<Option<String>>,
    draft: RefCell<String>,
    /// The network being joined.
    joining: RefCell<Option<String>>,
    /// The last join that failed, and why.
    failure: RefCell<Option<(String, String)>>,
    scanning: Cell<bool>,
    /// Turning the radio on or off failed, most likely for want of eclipse-priv.
    radio_failed: Cell<bool>,
}

impl NetworkPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            generation: Cell::new(0),
            status: RefCell::default(),
            shape: RefCell::default(),
            live: RefCell::default(),
            asking: RefCell::default(),
            draft: RefCell::default(),
            joining: RefCell::default(),
            failure: RefCell::default(),
            scanning: Cell::new(false),
            radio_failed: Cell::new(false),
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
        *self.asking.borrow_mut() = None;
        self.draft.borrow_mut().clear();
        *self.failure.borrow_mut() = None;
        self.radio_failed.set(false);
        self.shape.borrow_mut().clear();
        let network = self.clone();
        glib::spawn_future_local(async move {
            network.refresh().await;
            network.popout.show(Anchor::Under(x));
            network.rescan();
        });

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ticks = Cell::new(0);
        let network = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match network.upgrade() {
            Some(n) if n.popout.is_open() && n.generation.get() == generation => {
                ticks.set(ticks.get() + 1);
                if ticks.get() % SCAN_EVERY == 0 {
                    blocking(wifi::scan, |_| {});
                }
                glib::spawn_future_local(async move { n.refresh().await });
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    async fn refresh(self: &Rc<Self>) {
        let Ok(status) = gio::spawn_blocking(wifi::read).await else { return };
        *self.status.borrow_mut() = Rc::new(status);
        if *self.shape.borrow() == self.shape() {
            self.update_live();
        } else {
            self.rebuild();
        }
    }

    /// Everything the card's layout depends on.
    fn shape(&self) -> String {
        let status = self.status.borrow();
        let mut shape = format!(
            "{:?} {} {} {} {} {:?} {:?} {:?} {} {}|",
            status.iface,
            status.radio,
            status.reachable,
            status.wired.is_some(),
            status.joined().is_some(),
            self.asking.borrow(),
            self.joining.borrow(),
            self.failure.borrow(),
            self.scanning.get(),
            self.radio_failed.get(),
        );
        for n in &status.networks {
            shape += &format!("{} {} {} {}|", n.ssid, n.secured, n.known, n.connected);
        }
        shape
    }

    fn rebuild(self: &Rc<Self>) {
        *self.shape.borrow_mut() = self.shape();
        *self.live.borrow_mut() = Live::default();
        let s = self.popout.scale();
        let status = self.status.borrow().clone();
        self.popout.set_content(&popout::card(&self.body(s, &status)));
        self.update_live();
    }

    fn update_live(&self) {
        let status = self.status.borrow();
        let live = self.live.borrow();
        if let Some(label) = &live.wired {
            label.set_text(&wired_line(&status));
        }
        if let (Some(label), Some(joined)) = (&live.joined, status.joined()) {
            label.set_text(&joined_line(&status, joined));
        }
        for (ssid, glyph) in &live.signals {
            if let Some(n) = status.networks.iter().find(|n| &n.ssid == ssid) {
                glyph.set_text(wifi::signal_glyph(n.signal));
            }
        }
    }

    // ---- actions ---------------------------------------------------------

    fn rescan(self: &Rc<Self>) {
        if self.scanning.replace(true) {
            return;
        }
        self.rebuild();
        blocking(wifi::scan, |_| {});
        let network = Rc::downgrade(self);
        glib::timeout_add_local_once(SCAN_SPIN, move || {
            if let Some(n) = network.upgrade() {
                n.scanning.set(false);
                glib::spawn_future_local(async move { n.refresh().await });
            }
        });
    }

    /// A click on a network row.
    fn pick(self: &Rc<Self>, net: &Network) {
        if self.joining.borrow().is_some() {
            return;
        }
        if self.asking.borrow().as_deref() == Some(&net.ssid) {
            *self.asking.borrow_mut() = None;
            return self.rebuild();
        }
        *self.failure.borrow_mut() = None;
        if net.connected {
            let network = self.clone();
            return blocking(wifi::disconnect, move |_| {
                glib::spawn_future_local(async move { network.refresh().await });
            });
        }
        if net.known || !net.secured {
            *self.asking.borrow_mut() = None;
            return self.join(net.ssid.clone(), None);
        }
        *self.asking.borrow_mut() = Some(net.ssid.clone());
        self.draft.borrow_mut().clear();
        self.rebuild();
    }

    fn join(self: &Rc<Self>, ssid: String, password: Option<String>) {
        if self.joining.borrow().is_some() {
            return;
        }
        *self.joining.borrow_mut() = Some(ssid.clone());
        *self.failure.borrow_mut() = None;
        self.rebuild();
        let network = self.clone();
        let target = ssid.clone();
        blocking(
            move || wifi::connect(&target, password.as_deref()),
            move |result| {
                *network.joining.borrow_mut() = None;
                match result {
                    Ok(()) => {
                        *network.asking.borrow_mut() = None;
                        network.draft.borrow_mut().clear();
                    }
                    Err(why) => *network.failure.borrow_mut() = Some((ssid, why)),
                }
                glib::spawn_future_local(async move { network.refresh().await });
            },
        );
    }

    fn set_radio(self: &Rc<Self>, on: bool) {
        let network = self.clone();
        glib::spawn_future_local(async move {
            let argv = ["sudo", "-n", HELPER, "wifi", if on { "on" } else { "off" }].map(std::ffi::OsStr::new);
            let ok = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
                Ok(p) => p.wait_check_future().await.is_ok(),
                Err(_) => false,
            };
            network.radio_failed.set(!ok);
            network.refresh().await;
        });
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
        let palette = theme::palette();

        let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        header.append(&theme::filled_icon_glyph(if status.wired.is_some() { "lan" } else { "wifi" }, 16.0 * s, palette.accent));
        header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("NETWORK"));
        column.append(&header);

        let mut live = self.live.borrow_mut();
        if status.wired.is_some() {
            let (row, label) = link_line(s, "lan");
            live.wired = Some(label);
            column.append(&row);
        }
        if status.joined().is_some() {
            let (row, label) = link_line(s, "wifi");
            live.joined = Some(label);
            column.append(&row);
        }
        drop(live);

        if status.iface.is_some() {
            column.append(&self.wifi_section(s, status));
        } else if status.wired.is_none() {
            column.append(&note(s, "No network"));
        }
        column
    }

    fn wifi_section(self: &Rc<Self>, s: f64, status: &Status) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let section = gtk::Box::new(gtk::Orientation::Vertical, px(8.0));

        let head = gtk::Box::builder().spacing(px(12.0)).height_request(px(24.0)).build();
        let label = popout::micro_label(s, "Wi-Fi");
        label.set_hexpand(true);
        head.append(&label);
        if status.radio {
            let scanning = self.scanning.get();
            let refresh = theme::icon_glyph("refresh", 16.0 * s, if scanning { FLAME } else { ICON_DIM });
            if !scanning {
                let network = Rc::downgrade(self);
                on_click(&refresh, move || network.upgrade().into_iter().for_each(|n| n.rescan()));
                on_hover(&refresh, {
                    let refresh = refresh.downgrade();
                    move |inside| {
                        if let Some(r) = refresh.upgrade() {
                            r.set_attributes(theme::icon_glyph("refresh", 16.0 * s, if inside { CREAM } else { ICON_DIM }).attributes().as_ref());
                        }
                    }
                });
            }
            head.append(&refresh);
        }
        let network = Rc::downgrade(self);
        let on = status.radio;
        head.append(&popout::switch(s, on, true, move || network.upgrade().into_iter().for_each(|n| n.set_radio(!on))));
        section.append(&head);
        section.append(&popout::divider());

        if self.radio_failed.get() {
            section.append(&note(s, "Run install.sh to turn Wi-Fi on or off."));
        }
        if !status.radio {
            section.append(&note(s, "Wi-Fi is off"));
            return section;
        }
        if !status.reachable {
            section.append(&note(s, "wpa_supplicant isn't running"));
            return section;
        }
        if status.networks.is_empty() {
            section.append(&note(s, "Searching for networks…"));
            return section;
        }

        let list = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
        for net in &status.networks {
            list.append(&self.network_row(s, net));
            if self.asking.borrow().as_deref() == Some(&net.ssid) {
                list.append(&self.password_row(s, &net.ssid));
            }
            if let Some((ssid, why)) = &*self.failure.borrow()
                && ssid == &net.ssid
            {
                let failed = TextStyle::new(9.5 * s, pango::Weight::Medium, theme::palette().accent_lit).label(why);
                failed.set_xalign(0.0);
                failed.set_wrap(true);
                failed.set_margin_start(px(10.0));
                list.append(&failed);
            }
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(px(220.0))
            .child(&list)
            .build();
        section.append(&scroller);
        section
    }

    fn network_row(self: &Rc<Self>, s: f64, net: &Network) -> gtk::Overlay {
        let px = |v: f64| (v * s).round() as i32;
        let palette = theme::palette();
        let hovered = Rc::new(Cell::new(false));
        let background = gtk::DrawingArea::new();
        let connected = net.connected;
        background.set_draw_func({
            let hovered = hovered.clone();
            move |_, cr, w, h| {
                let palette = theme::palette();
                let fill = if connected { palette.accent.alpha(0.14) } else if hovered.get() { palette.raised } else { return };
                theme::rounded_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), 8.0 * s);
                theme::set_source(cr, fill);
                let _ = cr.fill();
            }
        });

        let content = gtk::Box::builder().spacing(px(7.0)).height_request(px(30.0)).margin_start(px(10.0)).margin_end(px(10.0)).build();
        let (weight, color) = if connected { (pango::Weight::Semibold, palette.accent_lit) } else { (pango::Weight::Medium, SUBTLE) };
        let name = TextStyle::new(11.5 * s, weight, color).label(&net.ssid);
        popout::fill(&name);
        content.append(&name);
        if self.joining.borrow().as_deref() == Some(&net.ssid) {
            content.append(&TextStyle::new(9.5 * s, pango::Weight::Semibold, FLAME).label("Joining…"));
        }
        if net.secured {
            content.append(&theme::filled_icon_glyph("lock", 11.0 * s, ICON_DIM));
        }
        let signal = theme::filled_icon_glyph(wifi::signal_glyph(net.signal), 15.0 * s, if connected { palette.accent_lit } else { SUBTLE });
        content.append(&signal);
        self.live.borrow_mut().signals.push((net.ssid.clone(), signal));

        let row = gtk::Overlay::new();
        row.set_child(Some(&background));
        row.add_overlay(&content);
        row.set_measure_overlay(&content, true);
        on_hover(&row, move |inside| {
            hovered.set(inside);
            background.queue_draw();
        });
        let (network, net) = (Rc::downgrade(self), net.clone());
        on_click(&row, move || network.upgrade().into_iter().for_each(|n| n.pick(&net)));
        row
    }

    /// The password for `ssid`, typed in place; Enter or the return glyph joins.
    fn password_row(self: &Rc<Self>, s: f64, ssid: &str) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let row = gtk::Box::builder().spacing(px(7.0)).height_request(px(30.0)).margin_start(px(10.0)).margin_end(px(10.0)).build();
        let field = gtk::Text::builder().visibility(false).hexpand(true).placeholder_text("Password").build();
        field.set_attributes(Some(&TextStyle::new(11.5 * s, pango::Weight::Medium, CREAM).attrs()));
        field.set_text(&self.draft.borrow());
        field.set_position(-1);
        let network = Rc::downgrade(self);
        field.connect_changed(move |f| {
            if let Some(n) = network.upgrade() {
                *n.draft.borrow_mut() = f.text().into();
            }
        });
        let submit = {
            let (network, ssid) = (Rc::downgrade(self), ssid.to_owned());
            Rc::new(move || {
                if let Some(n) = network.upgrade() {
                    let password = n.draft.borrow().clone();
                    if !password.is_empty() {
                        n.join(ssid.clone(), Some(password));
                    }
                }
            })
        };
        field.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
        row.append(&field);
        let enter = theme::icon_glyph("keyboard_return", 14.0 * s, theme::palette().accent_lit);
        on_click(&enter, move || submit());
        row.append(&enter);
        // Focus once the card is on screen.
        let field = field.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(f) = field.upgrade() {
                f.grab_focus();
            }
        });
        row
    }
}

/// Runs `work` on a worker thread and hands its result to `done` back on the main thread.
fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    glib::spawn_future_local(async move {
        if let Ok(result) = gio::spawn_blocking(work).await {
            done(result);
        }
    });
}

/// A link's glyph and a line about it, both in the accent.
fn link_line(s: f64, glyph: &str) -> (gtk::Box, gtk::Label) {
    let px = |v: f64| (v * s).round() as i32;
    let lit = theme::palette().accent_lit;
    let row = gtk::Box::builder().spacing(px(11.0)).height_request(px(30.0)).margin_start(px(2.0)).build();
    row.append(&theme::filled_icon_glyph(glyph, 15.0 * s, lit));
    let label = TextStyle::new(10.5 * s, pango::Weight::Semibold, lit).label("");
    popout::fill(&label);
    row.append(&label);
    (row, label)
}

fn wired_line(status: &Status) -> String {
    let mut parts = vec!["Ethernet".to_owned()];
    if let Some(Some(speed)) = status.wired {
        parts.push(if speed >= 1000 { format!("{} Gb/s", f64::from(speed) / 1000.0) } else { format!("{speed} Mb/s") });
    }
    parts.extend(status.wired_ip.clone());
    parts.join(" · ")
}

fn joined_line(status: &Status, joined: &Network) -> String {
    let mut parts = vec![joined.ssid.clone(), format!("{}%", (joined.signal * 100.0).round())];
    parts.extend(status.wifi_ip.clone());
    parts.join(" · ")
}

fn note(s: f64, text: &str) -> gtk::Label {
    let label = TextStyle::new(10.5 * s, pango::Weight::Normal, FAINT).label(text);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}
