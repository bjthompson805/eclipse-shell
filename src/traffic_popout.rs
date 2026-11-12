//! Traffic popout, dropping from the net-speed readout: download and upload
//! now and at their peak, the last two minutes as a mirrored graph (download
//! above the line, upload below), and the bytes moved since the shell started
//! and since the interface came up.
//!
//! Everything follows src/traffic.rs's readings in place; the card is only
//! rebuilt when the traffic moves to another interface.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, Rgba, TextStyle, CREAM, DIM, FAINT, MOON_GLOW, SUBTLE};
use crate::traffic::{self, format_bytes, Rate, Traffic};
use crate::wifi;

const NAMESPACE: &str = "eclipse-shell-traffic";
const WIDTH: f64 = 320.0;
/// The graph never scales below this, so an idle link doesn't fill it with noise.
const FLOOR: f64 = 8.0 * 1024.0;

/// What a reading updates on the card.
struct Live {
    down: gtk::Label,
    up: gtk::Label,
    down_peak: gtk::Label,
    up_peak: gtk::Label,
    /// Down and up, for each total.
    since_start: (gtk::Label, gtk::Label),
    since_boot: (gtk::Label, gtk::Label),
    graph: gtk::DrawingArea,
}

pub struct TrafficPopout {
    popout: Rc<Popout>,
    iface: RefCell<String>,
    /// The joined Wi-Fi network, to name the link by.
    network: RefCell<String>,
    live: RefCell<Option<Live>>,
    /// The graph's scale for each half, eased toward the recent peak.
    scale: Rc<Cell<(f64, f64)>>,
}

impl TrafficPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let popout = Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            iface: RefCell::default(),
            network: RefCell::default(),
            live: RefCell::default(),
            scale: Rc::new(Cell::new((FLOOR, FLOOR))),
        });
        let weak = Rc::downgrade(&popout);
        traffic::on_change(move || {
            if let Some(p) = weak.upgrade().filter(|p| p.popout.is_open()) {
                p.update();
            }
        });
        popout
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let t = traffic::current();
        let peaks = peaks(&t);
        self.scale.set((peaks.down.max(FLOOR), peaks.up.max(FLOOR)));
        self.network.borrow_mut().clear();
        self.rebuild(&t);
        self.popout.show(Anchor::Under(x));

        // Name the Wi-Fi network once it's known.
        if t.iface.starts_with("wlan") {
            let popout = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let Ok(status) = gio::spawn_blocking(wifi::read).await else { return };
                let Some(p) = popout.upgrade() else { return };
                if let Some(joined) = status.joined() {
                    *p.network.borrow_mut() = joined.ssid.clone();
                    p.rebuild(&traffic::current());
                }
            });
        }
    }

    fn update(&self) {
        let t = traffic::current();
        if *self.iface.borrow() != t.iface {
            return self.rebuild(&t);
        }
        let Some(live) = &*self.live.borrow() else { return };
        let now = t.now.unwrap_or_default();
        let peak = peaks(&t);
        live.down.set_text(&format_bytes(now.down, true));
        live.up.set_text(&format_bytes(now.up, true));
        live.down_peak.set_text(&format!("peak {}", format_bytes(peak.down, true)));
        live.up_peak.set_text(&format!("peak {}", format_bytes(peak.up, true)));
        for ((down, up), (moved_down, moved_up)) in [(&live.since_start, t.since_start), (&live.since_boot, t.since_boot)] {
            down.set_text(&format!("\u{2193} {}", format_bytes(moved_down as f64, false)));
            up.set_text(&format!("\u{2191} {}", format_bytes(moved_up as f64, false)));
        }
        // Ease each half's scale toward its peak: quickly up, slowly down.
        let ease = |scale: f64, target: f64| {
            let target = target.max(FLOOR);
            if target > scale { scale + (target - scale) * 0.6 } else { scale + (target - scale) * 0.15 }
        };
        let (down, up) = self.scale.get();
        self.scale.set((ease(down, peak.down), ease(up, peak.up)));
        live.graph.queue_draw();
    }

    fn rebuild(&self, t: &Traffic) {
        *self.iface.borrow_mut() = t.iface.clone();
        let s = self.popout.scale();
        let px = |v: f64| (v * s).round() as i32;
        let palette = theme::palette();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(12.0))
            .width_request(px(WIDTH) - 2 * px(16.0))
            .margin_top(px(13.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();

        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        head.append(&theme::filled_icon_glyph("swap_vert", 16.0 * s, palette.accent));
        let title = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("TRAFFIC");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        head.append(&title);
        head.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(&self.link_name(t)));
        column.append(&head);

        if t.iface.is_empty() {
            let none = TextStyle::new(10.5 * s, pango::Weight::Normal, FAINT).label("No network");
            none.set_xalign(0.0);
            column.append(&none);
            self.popout.set_content(&popout::card(&column));
            *self.live.borrow_mut() = None;
            return;
        }

        let readouts = gtk::Box::builder().homogeneous(true).spacing(px(12.0)).build();
        let (down, down_peak) = readout(s, "arrow_downward", "Down", palette.accent_lit);
        let (up, up_peak) = readout(s, "arrow_upward", "Up", MOON_GLOW);
        readouts.append(&down.0);
        readouts.append(&up.0);
        column.append(&readouts);

        let graph = gtk::DrawingArea::builder().height_request(px(104.0)).hexpand(true).build();
        graph.set_draw_func({
            let scale = self.scale.clone();
            move |_, cr, w, h| {
                let t = traffic::current();
                draw_graph(cr, f64::from(w), f64::from(h), &t, scale.get(), s);
            }
        });
        column.append(&graph);
        let ends = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let ago = TextStyle::new(9.0 * s, pango::Weight::Medium, FAINT).mono().label("2 min ago");
        ago.set_hexpand(true);
        ago.set_xalign(0.0);
        ends.append(&ago);
        ends.append(&TextStyle::new(9.0 * s, pango::Weight::Medium, FAINT).mono().label("now"));
        column.append(&ends);

        column.append(&popout::divider());
        // A grid, so both rows' figures line up in columns.
        let totals = gtk::Grid::builder().column_spacing(px(14.0)).row_spacing(px(8.0)).build();
        let since_start = total_row(s, &totals, 0, "Since the shell started");
        let since_boot = total_row(s, &totals, 1, &format!("Since {} came up", t.iface));
        column.append(&totals);

        self.popout.set_content(&popout::card(&column));
        *self.live.borrow_mut() = Some(Live { down: down.1, up: up.1, down_peak, up_peak, since_start, since_boot, graph: graph.clone() });
        self.update();
    }

    /// "wlan0 · Pixel_6333", or just the interface.
    fn link_name(&self, t: &Traffic) -> String {
        let network = self.network.borrow();
        if network.is_empty() { t.iface.clone() } else { format!("{} \u{b7} {}", t.iface, network) }
    }
}

fn peaks(t: &Traffic) -> Rate {
    t.history.iter().fold(Rate::default(), |p, r| Rate { down: p.down.max(r.down), up: p.up.max(r.up) })
}


/// A direction's heading, its rate large in its colour, and its peak.
/// Returns the column with its rate label, and the peak label.
fn readout(s: f64, glyph: &str, name: &str, color: Rgba) -> ((gtk::Box, gtk::Label), gtk::Label) {
    let px = |v: f64| (v * s).round() as i32;
    let column = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    let head = gtk::Box::new(gtk::Orientation::Horizontal, px(4.0));
    head.append(&theme::filled_icon_glyph(glyph, 12.0 * s, color));
    head.append(&popout::micro_label(s, name));
    column.append(&head);
    let rate = TextStyle::new(19.0 * s, pango::Weight::Semibold, CREAM).label("");
    rate.set_xalign(0.0);
    column.append(&rate);
    let peak = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).label("");
    peak.set_xalign(0.0);
    column.append(&peak);
    ((column, rate), peak)
}

/// A total's name, then its download and upload in their own columns.
fn total_row(s: f64, grid: &gtk::Grid, row: i32, name: &str) -> (gtk::Label, gtk::Label) {
    let label = popout::micro_label(s, name);
    label.set_hexpand(true);
    grid.attach(&label, 0, row, 1, 1);
    let value = || {
        let value = TextStyle::new(10.5 * s, pango::Weight::Semibold, CREAM).label("");
        value.set_xalign(1.0);
        value
    };
    let (down, up) = (value(), value());
    grid.attach(&down, 1, row, 1, 1);
    grid.attach(&up, 2, row, 1, 1);
    (down, up)
}

/// Download rising above the midline and upload hanging below it, each
/// scaled to its own recent peak, newest at the right.
fn draw_graph(cr: &cairo::Context, w: f64, h: f64, t: &Traffic, (down_scale, up_scale): (f64, f64), s: f64) {
    let palette = theme::palette();
    let mid = h / 2.0;
    let reach = mid - 3.0 * s;
    let step = w / (traffic::HISTORY - 1) as f64;
    // Right-aligned: the newest sample sits at the right edge.
    let start = w - step * (t.history.len().max(1) - 1) as f64;
    let points = |pick: fn(&Rate) -> f64, scale: f64, sign: f64| -> Vec<(f64, f64)> {
        t.history.iter().enumerate().map(|(i, r)| (start + step * i as f64, mid - sign * (pick(r) / scale).min(1.0) * reach)).collect()
    };

    for (points, color, sign) in [(points(|r| r.down, down_scale, 1.0), palette.accent_lit, 1.0), (points(|r| r.up, up_scale, -1.0), MOON_GLOW, -1.0)] {
        if points.len() < 2 {
            continue;
        }
        cr.move_to(points[0].0, mid);
        for &(x, y) in &points {
            cr.line_to(x, y);
        }
        cr.line_to(points[points.len() - 1].0, mid);
        cr.close_path();
        let wash = cairo::LinearGradient::new(0.0, mid - sign * reach, 0.0, mid);
        wash.add_color_stop_rgba(0.0, color.r, color.g, color.b, 0.35);
        wash.add_color_stop_rgba(1.0, color.r, color.g, color.b, 0.04);
        let _ = cr.set_source(&wash);
        let _ = cr.fill();
        cr.move_to(points[0].0, points[0].1);
        for &(x, y) in &points[1..] {
            cr.line_to(x, y);
        }
        cr.set_line_width(1.5 * s);
        cr.set_line_join(cairo::LineJoin::Round);
        theme::set_source(cr, color);
        let _ = cr.stroke();
    }

    cr.move_to(0.0, mid);
    cr.line_to(w, mid);
    cr.set_line_width(1.0);
    theme::set_source(cr, theme::HAIR);
    let _ = cr.stroke();

    // Each half's scale, in its corner.
    for (scale, y) in [(down_scale, 0.0), (up_scale, h - 13.0 * s)] {
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_text(&format_bytes(scale, true));
        layout.set_attributes(Some(&TextStyle::new(8.5 * s, pango::Weight::Medium, FAINT).mono().attrs()));
        cr.move_to(2.0 * s, y);
        pangocairo::functions::show_layout(cr, &layout);
    }
}
