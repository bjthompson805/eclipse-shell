//! Resources popout, dropping from the bar's stats readout: CPU with a
//! sparkline and a bar per core, GPU and its engines, memory, temperature,
//! disk use and throughput, and the busiest processes. Rebuilt on each
//! two-second reading while open.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, pango};
use gtk4 as gtk;

use crate::popout::{self, Anchor, Popout};
use crate::sysstats::{self, Stats, HISTORY};
use crate::theme::{self, Rgba, TextStyle, CREAM, DIM, HAIR, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-resources";
/// Disk throughput that fills a rate bar, in bytes per second.
const DISK_FULL_RATE: f64 = 150.0 * 1024.0 * 1024.0;
const CORE_CELL: f64 = 52.5;
const GAP: f64 = 6.0;

pub struct ResourcesPopout {
    popout: Rc<Popout>,
}

impl ResourcesPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let resources = Rc::new(Self { popout: Popout::new(app, NAMESPACE) });
        sysstats::on_change({
            let resources = Rc::downgrade(&resources);
            move || {
                let Some(r) = resources.upgrade() else { return };
                if r.popout.is_open() {
                    r.rebuild();
                } else {
                    sysstats::want_processes(false);
                }
            }
        });
        resources
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        sysstats::want_processes(true);
        self.rebuild();
        self.popout.show(Anchor::Under(x));
    }

    fn rebuild(&self) {
        let s = self.popout.scale();
        self.popout.set_content(&popout::card(&body(s, &sysstats::current())));
    }
}

/// Core columns: four to eight, about three rows' worth.
fn core_columns(cores: usize) -> usize {
    cores.div_ceil(3).clamp(4, 8)
}

fn warn_color(warn: bool) -> Rgba {
    let palette = theme::palette();
    if warn { palette.accent_lit } else { palette.accent }
}

fn body(s: f64, stats: &Stats) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let columns = core_columns(stats.cores.len());
    let inner = (columns as f64 * CORE_CELL + (columns - 1) as f64 * GAP) * s;
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(px(11.0))
        .width_request(inner.round() as i32)
        .margin_top(px(13.0))
        .margin_bottom(px(14.0))
        .margin_start(px(16.0))
        .margin_end(px(16.0))
        .build();

    let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    header.append(&theme::filled_icon_glyph("monitor_heart", 16.0 * s, theme::palette().accent));
    header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("RESOURCES"));
    column.append(&header);

    if let Some(cpu) = stats.cpu {
        column.append(&metric(s, "CPU", &format!("{cpu}%"), &stats.cpu_history, 100.0, cpu > 85));
        if !stats.cores.is_empty() {
            let grid = gtk::Grid::builder().column_spacing(px(GAP)).row_spacing(px(8.0)).column_homogeneous(true).build();
            for (i, load) in stats.cores.iter().enumerate() {
                grid.attach(&cell(s, &format!("C{i}"), &format!("{load}%"), f64::from(*load) / 100.0, *load > 85, 8.0), (i % columns) as i32, (i / columns) as i32, 1, 1);
            }
            column.append(&grid);
        }
    }
    if let Some(gpu) = stats.gpu {
        column.append(&metric(s, "GPU", &format!("{gpu}%"), &stats.gpu_history, 100.0, gpu > 85));
        let engines = gtk::Box::builder().spacing(px(GAP)).homogeneous(true).build();
        engines.append(&cell(s, "GFX", &format!("{gpu}%"), f64::from(gpu) / 100.0, gpu > 85, 7.5));
        if let Some(video) = stats.video {
            engines.append(&cell(s, "VIDEO", &format!("{video}%"), f64::from(video) / 100.0, video > 85, 7.5));
        }
        column.append(&engines);
    }
    if let Some(mem) = stats.mem {
        column.append(&metric(s, "Memory", &format!("{mem}%"), &stats.mem_history, 100.0, mem > 90));
    }
    if let Some(temp) = stats.temp {
        column.append(&metric(s, "Temp", &format!("{temp}\u{b0}C"), &stats.temp_history, 100.0, temp > 80));
    }
    if let Some(disk) = &stats.disk {
        let section = gtk::Box::new(gtk::Orientation::Vertical, px(6.0));
        section.append(&metric_head(s, "Disk", &format!("{}%", disk.percent), disk.percent > 90));
        section.append(&bar(s, 5.0, f64::from(disk.percent) / 100.0, warn_color(disk.percent > 90)));
        let used = TextStyle::new(9.0 * s, pango::Weight::Medium, DIM).label(&format!("{} / {}", disk.used, disk.total));
        used.set_xalign(1.0);
        section.append(&used);
        if let (Some(read), Some(write)) = (stats.disk_read, stats.disk_write) {
            let io = gtk::Box::builder().spacing(px(GAP)).homogeneous(true).build();
            io.append(&cell(s, "READ", &rate(read), read / DISK_FULL_RATE, false, 7.5));
            io.append(&cell(s, "WRITE", &rate(write), write / DISK_FULL_RATE, false, 7.5));
            section.append(&io);
        }
        column.append(&section);
    }

    if !stats.processes.is_empty() {
        column.append(&divider());
        let header = TextStyle::new(9.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.4 * s).label("TOP PROCESSES");
        header.set_xalign(0.0);
        column.append(&header);
        let list = gtk::Box::new(gtk::Orientation::Vertical, px(5.0));
        for (cpu, name) in &stats.processes {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let percent = TextStyle::new(10.0 * s, pango::Weight::Medium, SUBTLE).mono().label(&format!("{cpu:.1}%"));
            percent.set_xalign(0.0);
            percent.set_size_request(px(40.0) + px(8.0), -1);
            row.append(&percent);
            let label = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(name);
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(pango::EllipsizeMode::End);
            row.append(&label);
            list.append(&row);
        }
        column.append(&list);
    }
    column
}

/// "12.3 MB/s", "840 KB/s", "0 B/s".
fn rate(bytes_per_sec: f64) -> String {
    const K: f64 = 1024.0;
    match bytes_per_sec {
        b if b >= K * K => format!("{:.1} MB/s", b / (K * K)),
        b if b >= K => format!("{:.0} KB/s", b / K),
        b => format!("{} B/s", b.round()),
    }
}

/// A metric's caps label with its value on the right.
fn metric_head(s: f64, label: &str, value: &str, warn: bool) -> gtk::Box {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let name = TextStyle::new(9.0 * s, pango::Weight::Semibold, SUBTLE).mono().tracking(1.2 * s).label(&label.to_uppercase());
    name.set_hexpand(true);
    name.set_xalign(0.0);
    name.set_valign(gtk::Align::Baseline);
    head.append(&name);
    let value = TextStyle::new(14.0 * s, pango::Weight::Semibold, if warn { theme::palette().accent_lit } else { CREAM }).label(value);
    value.set_valign(gtk::Align::Baseline);
    head.append(&value);
    head
}

/// A metric: its head over a sparkline of recent readings, newest on the
/// right and brightest.
fn metric(s: f64, label: &str, value: &str, history: &std::collections::VecDeque<f64>, max: f64, warn: bool) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, (6.0 * s).round() as i32);
    section.append(&metric_head(s, label, value, warn));
    let series: Vec<f64> = history.iter().copied().collect();
    let spark = gtk::DrawingArea::builder().content_height((18.0 * s).round() as i32).hexpand(true).build();
    let color = warn_color(warn);
    spark.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        let shown = series.len().min(HISTORY);
        if shown == 0 {
            return;
        }
        let gap = 1.5 * s;
        let bar_w = (w - (shown - 1) as f64 * gap) / shown as f64;
        for (i, v) in series[series.len() - shown..].iter().enumerate() {
            let height = (h * (v / max).min(1.0)).max(1.0);
            let x = i as f64 * (bar_w + gap);
            theme::rounded_rect(cr, x, h - height, bar_w, height, (bar_w / 2.0).min(height / 2.0));
            let fade = 0.35 + 0.65 * i as f64 / (shown - 1).max(1) as f64;
            theme::set_source(cr, color.alpha(color.a * fade));
            let _ = cr.fill();
        }
    });
    section.append(&spark);
    section
}

/// A small labelled bar: caps name, the bar, the value.
fn cell(s: f64, name: &str, value: &str, fraction: f64, warn: bool, name_px: f64) -> gtk::Box {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, (3.0 * s).round() as i32);
    let label = TextStyle::new(name_px * s, pango::Weight::Semibold, DIM).mono().label(name);
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    cell.append(&label);
    cell.append(&bar(s, 5.0, fraction, warn_color(warn)));
    let value = TextStyle::new(8.5 * s, pango::Weight::Medium, if warn { theme::palette().accent_lit } else { SUBTLE }).label(value);
    value.set_xalign(0.0);
    cell.append(&value);
    cell
}

fn bar(s: f64, height: f64, fraction: f64, fill: Rgba) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_height((height * s).round() as i32).hexpand(true).build();
    area.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
        theme::set_source(cr, HAIR);
        let _ = cr.fill();
        let filled = w * fraction.clamp(0.0, 1.0);
        if filled > 0.0 {
            theme::rounded_rect(cr, 0.0, 0.0, filled.max(h), h, h / 2.0);
            theme::set_source(cr, fill);
            let _ = cr.fill();
        }
    });
    area
}

fn divider() -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_height(1).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, HAIR);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line
}
