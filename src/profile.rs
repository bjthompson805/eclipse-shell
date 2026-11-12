//! The left sidebar: this machine at a glance. A system card (who and where,
//! the mark in an orbit window, OS, desktop, memory and disk, CPU and GPU)
//! over a field sheet (vitals, runtime, packages, look, palette).
//!
//! The fixed facts are gathered once, on a worker thread, the first time the
//! sidebar opens; the live figures follow the shared sysstats poller while it
//! is open.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::process::Command;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use gtk4 as gtk;

use crate::sidebar::{Side, Sidebar};
use crate::sysstats::{self, Stats};
use crate::theme::{self, Rgba, TextStyle, CREAM, DIM, FAINT, HAIR, SUBTLE};
use crate::{config, mark, popout, wallpaper};

/// What doesn't change while the shell runs.
#[derive(Clone, Default)]
struct Facts {
    user: String,
    host: String,
    os: String,
    kernel: String,
    arch: String,
    cpu: String,
    cores: u32,
    gpu: String,
    /// The first monitor's mode, in device pixels.
    screen: String,
    memory: f64,
    desktop: String,
    shell: String,
    packages: u32,
    yours: u32,
    booted: i64,
}

pub struct Profile {
    sidebar: Rc<Sidebar>,
    facts: RefCell<Option<Facts>>,
    /// Live readouts of the open content, fed by each sysstats sample.
    updaters: RefCell<Vec<Box<dyn Fn(&Stats)>>>,
    open: Cell<bool>,
}

impl Profile {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let sidebar = Sidebar::new(app, Side::Left, "eclipse-shell-profile", 340.0);
        let profile = Rc::new(Self { sidebar, facts: RefCell::default(), updaters: RefCell::default(), open: Cell::new(false) });
        // Gathered now, so the first open has them to show as it slides in.
        glib::spawn_future_local({
            let profile = Rc::downgrade(&profile);
            async move {
                let facts = gio::spawn_blocking(gather).await.unwrap_or_default();
                if let Some(p) = profile.upgrade() {
                    if p.facts.borrow().is_none() {
                        *p.facts.borrow_mut() = Some(facts);
                    }
                }
            }
        });
        profile.sidebar.connect_open({
            let profile = Rc::downgrade(&profile);
            move |open, s| {
                let Some(p) = profile.upgrade() else { return };
                p.open.set(open);
                if open {
                    p.build(s);
                } else {
                    p.updaters.borrow_mut().clear();
                }
            }
        });
        sysstats::on_change({
            let profile = Rc::downgrade(&profile);
            move || {
                if let Some(p) = profile.upgrade().filter(|p| p.open.get()) {
                    let stats = sysstats::current();
                    p.updaters.borrow().iter().for_each(|f| f(&stats));
                }
            }
        });
        profile
    }

    pub fn sidebar(&self) -> &Rc<Sidebar> {
        &self.sidebar
    }

    fn build(self: &Rc<Self>, s: f64) {
        let Some(facts) = self.facts.borrow().clone() else {
            // First open: gather, then build with the facts in hand.
            let profile = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let facts = gio::spawn_blocking(gather).await.unwrap_or_default();
                if let Some(p) = profile.upgrade() {
                    *p.facts.borrow_mut() = Some(facts);
                    if p.open.get() {
                        p.build(p.sidebar.scale());
                    }
                }
            });
            return;
        };
        let px = |v: f64| (v * s).round() as i32;
        let mut updaters: Vec<Box<dyn Fn(&Stats)>> = Vec::new();

        let column = gtk::Box::new(gtk::Orientation::Vertical, px(22.0));
        column.set_margin_start(px(16.0));
        column.set_margin_end(px(12.0));
        column.set_margin_top(px(16.0));
        column.set_margin_bottom(px(18.0));
        column.append(&card(s, &facts, &mut updaters));
        column.append(&sheet(s, &facts, &mut updaters));

        // Inset, so the scrollbar doesn't hide against the panel's edge.
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).hexpand(true).margin_end(px(6.0)).build();
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.append(&scroller);
        scroller.set_vexpand(true);
        self.sidebar.set_content(&holder);
        let stats = sysstats::current();
        updaters.iter().for_each(|f| f(&stats));
        *self.updaters.borrow_mut() = updaters;
    }
}

// ── The system card ─────────────────────────────────────────────────────────

fn card(s: f64, facts: &Facts, updaters: &mut Vec<Box<dyn Fn(&Stats)>>) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let body = gtk::Box::new(gtk::Orientation::Vertical, px(14.0));
    body.set_margin_start(px(16.0));
    body.set_margin_end(px(16.0));
    body.set_margin_top(px(16.0));
    body.set_margin_bottom(px(14.0));

    // Who and where, with memory as a ring.
    let head = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
    let names = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
    names.set_hexpand(true);
    let user = TextStyle::new(17.0 * s, pango::Weight::Bold, CREAM).label(&facts.user);
    user.set_xalign(0.0);
    names.append(&user);
    let sub = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).mono().tracking(0.8 * s).label(&format!("{} · {}", facts.host, facts.arch));
    sub.set_xalign(0.0);
    names.append(&sub);
    head.append(&names);
    let (ring, update_ring) = ring(s);
    head.append(&ring);
    updaters.push(Box::new(move |st| update_ring(st.mem.map(|m| f64::from(m) / 100.0))));
    body.append(&head);

    body.append(&orbit_window(s));

    // The type line: a SYSTEM chip, a rule, the OS.
    let kind = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    kind.append(&chip(s, "SYSTEM"));
    kind.append(&rule(true));
    let os = TextStyle::new(10.5 * s, pango::Weight::Semibold, SUBTLE).mono().label(&facts.os);
    kind.append(&os);
    body.append(&kind);

    // Desktop and shell, screen and packages, beside an accent bar.
    let flavour = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
    flavour.append(&accent_bar(s));
    let grid = gtk::Grid::builder().column_spacing(px(14.0)).row_spacing(px(5.0)).hexpand(true).build();
    for (i, (key, value)) in [("WM", facts.desktop.as_str()), ("SH", facts.shell.as_str()), ("RES", facts.screen.as_str()), ("PKG", &facts.packages.to_string())].into_iter().enumerate() {
        let pair = gtk::Box::new(gtk::Orientation::Horizontal, px(7.0));
        let k = TextStyle::new(9.0 * s, pango::Weight::Semibold, FAINT).mono().tracking(1.0 * s).label(key);
        k.set_width_chars(3);
        k.set_xalign(0.0);
        pair.append(&k);
        let v = TextStyle::new(10.5 * s, pango::Weight::Medium, CREAM).label(value);
        v.set_xalign(0.0);
        v.set_ellipsize(pango::EllipsizeMode::End);
        v.set_max_width_chars(1);
        v.set_hexpand(true);
        pair.append(&v);
        grid.attach(&pair, (i % 2) as i32, (i / 2) as i32, 1, 1);
    }
    grid.set_column_homogeneous(true);
    flavour.append(&grid);
    body.append(&flavour);

    // Memory and disk as labelled lines.
    let memory = facts.memory;
    body.append(&reading(s, "Memory", updaters, move |st| {
        let pct = f64::from(st.mem?) / 100.0;
        Some((format!("{} / {} · {:.0}%", gib(memory * pct), gib(memory), pct * 100.0), pct))
    }));
    body.append(&reading(s, "Disk", updaters, |st| {
        let d = st.disk.as_ref()?;
        Some((format!("{} / {} · {}%", d.used, d.total, d.percent), f64::from(d.percent) / 100.0))
    }));

    body.append(&popout::divider());

    // CPU | GPU.
    let chips = gtk::Box::new(gtk::Orientation::Horizontal, px(14.0));
    chips.set_homogeneous(true);
    let (cpu, set_cpu) = processor(s, "CPU", &short_cpu(&facts.cpu));
    let (gpu, set_gpu) = processor(s, "GPU", &short_gpu(&facts.gpu));
    updaters.push(Box::new(move |st| {
        set_cpu(st.cpu, st.temp);
        set_gpu(st.gpu, None);
    }));
    chips.append(&cpu);
    chips.append(&gpu);
    body.append(&chips);

    // Footer: uptime, then a row of accent dots.
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    let up = TextStyle::new(9.0 * s, pango::Weight::Semibold, FAINT).mono().tracking(1.2 * s).label("");
    up.set_xalign(0.0);
    up.set_hexpand(true);
    let booted = facts.booted;
    updaters.push(Box::new({
        let up = up.clone();
        move |_| up.set_text(&format!("UP {}", uptime(booted).to_uppercase()))
    }));
    foot.append(&up);
    foot.append(&dots(s));
    body.append(&foot);

    popout::card(&body)
}

/// The portrait: the mark in orbit over a dot grid, with corner ticks.
fn orbit_window(s: f64) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_height((118.0 * s).round() as i32).hexpand(true).build();
    area.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.0, 0.0, w, h, 6.0 * s);
        theme::set_source(cr, palette.recessed);
        let _ = cr.fill_preserve();
        let _ = cr.save();
        cr.clip();

        let step = 12.0 * s;
        theme::set_source(cr, HAIR);
        let mut y = step / 2.0;
        while y < h {
            let mut x = step / 2.0;
            while x < w {
                cr.rectangle(x, y, 1.0, 1.0);
                x += step;
            }
            y += step;
        }
        let _ = cr.fill();

        let size = 62.0 * s;
        let (cx, cy) = (w / 2.0, h / 2.0);
        let a = palette.accent;
        let glow = gtk::cairo::RadialGradient::new(cx, cy, 0.0, cx, cy, h * 0.9);
        glow.add_color_stop_rgba(0.0, a.r, a.g, a.b, 0.28);
        glow.add_color_stop_rgba(1.0, a.r, a.g, a.b, 0.0);
        let _ = cr.set_source(&glow);
        cr.paint().ok();

        // A tilted orbit, its far half behind the mark.
        let orbit = |cr: &gtk::cairo::Context, from: f64, to: f64| {
            let _ = cr.save();
            cr.translate(cx, cy);
            cr.rotate(-0.28);
            cr.scale(1.0, 0.3);
            cr.arc(0.0, 0.0, size * 1.05, from, to);
            let _ = cr.restore();
        };
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.accent_lit.alpha(0.35));
        orbit(cr, PI, 2.0 * PI);
        let _ = cr.stroke();
        let _ = cr.save();
        cr.translate(cx - size / 2.0, cy - size / 2.0);
        mark::draw(cr, size);
        let _ = cr.restore();
        theme::set_source(cr, palette.accent_lit.alpha(0.55));
        orbit(cr, 0.0, PI);
        let _ = cr.stroke();

        // A few fixed stars.
        theme::set_source(cr, CREAM.alpha(0.5));
        for (fx, fy, r) in [(0.12, 0.22, 1.1), (0.24, 0.74, 0.8), (0.81, 0.18, 1.0), (0.9, 0.66, 1.3), (0.68, 0.84, 0.7), (0.35, 0.12, 0.7)] {
            cr.arc(w * fx, h * fy, r * s, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
        let _ = cr.restore();

        // Corner registration ticks.
        let (inset, arm) = (6.0 * s, 8.0 * s);
        theme::set_source(cr, SUBTLE.alpha(0.5));
        for (x, y, dx, dy) in [(inset, inset, 1.0, 1.0), (w - inset, inset, -1.0, 1.0), (inset, h - inset, 1.0, -1.0), (w - inset, h - inset, -1.0, -1.0)] {
            cr.move_to(x, y + dy * arm);
            cr.line_to(x, y);
            cr.line_to(x + dx * arm, y);
        }
        let _ = cr.stroke();
    });
    theme::on_palette_change({
        let area = area.downgrade();
        move || area.upgrade().into_iter().for_each(|a| a.queue_draw())
    });
    area
}

/// A memory ring with the percentage inside; feed it a fraction.
fn ring(s: f64) -> (gtk::Overlay, impl Fn(Option<f64>)) {
    let size = (40.0 * s).round() as i32;
    let value = Rc::new(Cell::new(0.0_f64));
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).build();
    area.set_draw_func({
        let value = value.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
            let line = 3.0 * s;
            let r = cx.min(cy) - line / 2.0;
            cr.set_line_width(line);
            cr.arc(cx, cy, r, 0.0, 2.0 * PI);
            theme::set_source(cr, palette.raised);
            let _ = cr.stroke();
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.arc(cx, cy, r, -PI / 2.0, -PI / 2.0 + 2.0 * PI * value.get().clamp(0.0, 1.0));
            theme::set_source(cr, palette.accent_lit);
            let _ = cr.stroke();
        }
    });
    let label = TextStyle::new(9.5 * s, pango::Weight::Bold, CREAM).mono().label("");
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&label);
    overlay.set_valign(gtk::Align::Center);
    let update = move |fraction: Option<f64>| {
        value.set(fraction.unwrap_or(0.0));
        label.set_text(&fraction.map_or("–".into(), |f| format!("{:.0}", f * 100.0)));
        area.queue_draw();
    };
    (overlay, update)
}

/// A small outlined caps chip.
fn chip(s: f64, text: &str) -> gtk::Overlay {
    let label = TextStyle::new(8.5 * s, pango::Weight::Bold, theme::palette().accent_lit).mono().tracking(1.4 * s).label(text);
    label.set_margin_start((6.0 * s).round() as i32);
    label.set_margin_end((6.0 * s).round() as i32);
    label.set_margin_top((2.0 * s).round() as i32);
    label.set_margin_bottom((2.0 * s).round() as i32);
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let a = theme::palette().accent;
        theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 3.0 * s);
        theme::set_source(cr, a.alpha(0.14));
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, a.alpha(0.6));
        let _ = cr.stroke();
    });
    let chip = gtk::Overlay::new();
    chip.set_child(Some(&back));
    chip.add_overlay(&label);
    chip.set_measure_overlay(&label, true);
    chip.set_valign(gtk::Align::Center);
    chip
}

/// A hairline that fills the space it's given; dotted for spec lines.
fn rule(solid: bool) -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_height(1).hexpand(true).valign(gtk::Align::Center).build();
    line.set_draw_func(move |_, cr, w, _| {
        theme::set_source(cr, HAIR);
        if solid {
            cr.rectangle(0.0, 0.0, f64::from(w), 1.0);
        } else {
            let mut x = 0.0;
            while x < f64::from(w) {
                cr.rectangle(x, 0.0, 1.0, 1.0);
                x += 3.0;
            }
        }
        let _ = cr.fill();
    });
    line
}

fn accent_bar(s: f64) -> gtk::DrawingArea {
    let bar = gtk::DrawingArea::builder().content_width((2.0 * s).round().max(2.0) as i32).build();
    bar.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, theme::palette().accent);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    bar
}

fn dots(s: f64) -> gtk::DrawingArea {
    let r = 2.0 * s;
    let area = gtk::DrawingArea::builder().content_width((r * 2.0 * 5.0 + 4.0 * 4.0 * s).ceil() as i32).content_height((r * 2.0).ceil() as i32).valign(gtk::Align::Center).build();
    area.set_draw_func(move |_, cr, _, h| {
        let palette = theme::palette();
        for i in 0..5 {
            let x = r + f64::from(i) * (r * 2.0 + 4.0 * s);
            cr.arc(x, f64::from(h) / 2.0, r, 0.0, 2.0 * PI);
            theme::set_source(cr, palette.accent_lit.alpha(1.0 - f64::from(i) * 0.18));
            let _ = cr.fill();
        }
    });
    area
}

/// A meter line whose fill fraction can change.
fn meter(s: f64, height: f64) -> (gtk::DrawingArea, Rc<Cell<f64>>) {
    let fraction = Rc::new(Cell::new(0.0_f64));
    let bar = gtk::DrawingArea::builder().content_height((height * s).round() as i32).hexpand(true).build();
    bar.set_draw_func({
        let fraction = fraction.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
            theme::set_source(cr, palette.raised);
            let _ = cr.fill();
            let filled = (w * fraction.get().clamp(0.0, 1.0)).round();
            if filled > 0.0 {
                theme::rounded_rect(cr, 0.0, 0.0, filled.max(h), h, h / 2.0);
                theme::set_source(cr, palette.accent_lit);
                let _ = cr.fill();
            }
        }
    });
    (bar, fraction)
}

/// A label and its value over a meter, both from each sample via `read`.
fn reading(s: f64, name: &str, updaters: &mut Vec<Box<dyn Fn(&Stats)>>, read: impl Fn(&Stats) -> Option<(String, f64)> + 'static) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let block = gtk::Box::new(gtk::Orientation::Vertical, px(5.0));
    let top = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    let label = popout::micro_label(s, name);
    label.set_hexpand(true);
    top.append(&label);
    let value = TextStyle::new(10.0 * s, pango::Weight::Medium, SUBTLE).mono().label("–");
    top.append(&value);
    block.append(&top);
    let (bar, fraction) = meter(s, 4.0);
    block.append(&bar);
    updaters.push(Box::new(move |st| {
        if let Some((text, f)) = read(st) {
            value.set_text(&text);
            fraction.set(f);
            bar.queue_draw();
        }
    }));
    block
}

/// A CPU or GPU column: caps name, big load figure, the part's name.
fn processor(s: f64, name: &str, part: &str) -> (gtk::Box, impl Fn(Option<u32>, Option<u32>) + use<>) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, (3.0 * s).round() as i32);
    let head = gtk::Box::new(gtk::Orientation::Horizontal, (6.0 * s).round() as i32);
    let label = popout::micro_label(s, name);
    label.set_hexpand(true);
    head.append(&label);
    let side = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).mono().label("");
    head.append(&side);
    column.append(&head);
    let figure = TextStyle::new(20.0 * s, pango::Weight::Bold, CREAM).mono().label("–");
    figure.set_xalign(0.0);
    column.append(&figure);
    let part = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).label(part);
    part.set_xalign(0.0);
    part.set_ellipsize(pango::EllipsizeMode::End);
    part.set_max_width_chars(1);
    column.append(&part);
    let update = move |load: Option<u32>, temp: Option<u32>| {
        figure.set_text(&load.map_or("–".into(), |l| format!("{l}%")));
        side.set_text(&temp.map_or(String::new(), |t| format!("{t}°C")));
    };
    (column, update)
}

// ── The field sheet ─────────────────────────────────────────────────────────

fn sheet(s: f64, facts: &Facts, updaters: &mut Vec<Box<dyn Fn(&Stats)>>) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let sheet = gtk::Box::new(gtk::Orientation::Vertical, px(12.0));

    sheet.append(&popout::eyebrow(s, "Vitals"));
    let vitals = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let booted = facts.booted;
    let columns: [(&str, Box<dyn Fn(&Stats) -> String>); 4] = [
        ("CPU", Box::new(|st| st.cpu.map_or("–".into(), |v| format!("{v}%")))),
        ("Memory", Box::new(|st| st.mem.map_or("–".into(), |v| format!("{v}%")))),
        ("Temp", Box::new(|st| st.temp.map_or("–".into(), |v| format!("{v}°")))),
        ("Uptime", Box::new(move |_| uptime(booted))),
    ];
    for (i, (name, read)) in columns.into_iter().enumerate() {
        if i > 0 {
            let split = gtk::DrawingArea::builder().content_width(1).margin_start(px(10.0)).margin_end(px(10.0)).build();
            split.set_draw_func(|_, cr, w, h| {
                theme::set_source(cr, HAIR);
                cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
                let _ = cr.fill();
            });
            vitals.append(&split);
        }
        let column = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
        column.set_hexpand(true);
        let figure = TextStyle::new(15.0 * s, pango::Weight::Bold, CREAM).mono().label("–");
        figure.set_xalign(0.0);
        column.append(&figure);
        column.append(&popout::micro_label(s, name));
        vitals.append(&column);
        updaters.push(Box::new(move |st| figure.set_text(&read(st))));
    }
    sheet.append(&vitals);

    sheet.append(&gap(s));
    sheet.append(&popout::eyebrow(s, "Runtime"));
    for (name, value) in [
        ("Kernel", format!("{} {}", facts.kernel, facts.arch)),
        ("Desktop", facts.desktop.clone()),
        ("Shell", facts.shell.clone()),
        ("Processor", short_cpu(&facts.cpu)),
        ("Threads", facts.cores.to_string()),
        ("Memory", gib(facts.memory)),
    ] {
        sheet.append(&spec(s, name, &value));
    }

    sheet.append(&gap(s));
    sheet.append(&popout::eyebrow(s, "Packages"));
    let packages = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    let total = TextStyle::new(15.0 * s, pango::Weight::Bold, CREAM).mono().label(&facts.packages.to_string());
    packages.append(&total);
    let rest = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(&format!("installed · {} by you", facts.yours));
    rest.set_valign(gtk::Align::Baseline);
    total.set_valign(gtk::Align::Baseline);
    packages.append(&rest);
    sheet.append(&packages);
    let (bar, fraction) = meter(s, 4.0);
    fraction.set(if facts.packages > 0 { f64::from(facts.yours) / f64::from(facts.packages) } else { 0.0 });
    sheet.append(&bar);

    sheet.append(&gap(s));
    sheet.append(&popout::eyebrow(s, "Look"));
    sheet.append(&spec(s, "Font", config::get().font.as_str()));
    sheet.append(&spec(s, "Icons", config::get().icon_theme.as_str()));
    let wall = spec(s, "Wallpaper", "–");
    let value = wall.last_child().and_downcast::<gtk::Label>();
    glib::spawn_future_local(async move {
        let name = wallpaper::current().await.and_then(|p| std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned()));
        if let (Some(value), Some(name)) = (value, name) {
            value.set_text(&name);
        }
    });
    sheet.append(&wall);
    sheet.append(&swatches(s));
    sheet
}

fn gap(s: f64) -> gtk::Box {
    let gap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gap.set_size_request(-1, (4.0 * s).round() as i32);
    gap
}

/// A spec line: name, a dotted rule taking up the slack, value.
fn spec(s: f64, name: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (8.0 * s).round() as i32);
    let name = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).mono().label(name);
    row.append(&name);
    row.append(&rule(false));
    let value = TextStyle::new(10.5 * s, pango::Weight::Medium, CREAM).label(value);
    value.set_ellipsize(pango::EllipsizeMode::Start);
    value.set_max_width_chars(28);
    row.append(&value);
    row
}

/// The wallust palette's sixteen colours as one strip.
fn swatches(s: f64) -> gtk::DrawingArea {
    let colours = Rc::new(RefCell::new(wallust_colours()));
    let area = gtk::DrawingArea::builder().content_height((22.0 * s).round() as i32).hexpand(true).build();
    area.set_draw_func({
        let colours = colours.clone();
        move |_, cr, w, h| {
            let colours = colours.borrow();
            if colours.is_empty() {
                return;
            }
            let (w, h) = (f64::from(w), f64::from(h));
            let _ = cr.save();
            theme::rounded_rect(cr, 0.0, 0.0, w, h, 5.0 * s);
            cr.clip();
            let cell = w / colours.len() as f64;
            for (i, c) in colours.iter().enumerate() {
                cr.rectangle((i as f64 * cell).floor(), 0.0, cell.ceil() + 1.0, h);
                theme::set_source(cr, *c);
                let _ = cr.fill();
            }
            let _ = cr.restore();
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 5.0 * s);
            cr.set_line_width(1.0);
            theme::set_source(cr, HAIR);
            let _ = cr.stroke();
        }
    });
    theme::on_palette_change({
        let area = area.downgrade();
        move || {
            if let Some(area) = area.upgrade() {
                *colours.borrow_mut() = wallust_colours();
                area.queue_draw();
            }
        }
    });
    area
}

fn wallust_colours() -> Vec<Rgba> {
    let cache = std::env::var_os("XDG_CACHE_HOME").map(Into::into).unwrap_or_else(|| glib::home_dir().join(".cache"));
    let Ok(text) = std::fs::read_to_string(cache.join("wallust").join("colors.json")) else { return Vec::new() };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    (0..16).filter_map(|i| json.get(format!("color{i}"))?.as_str().and_then(Rgba::hex)).collect()
}

// ── Facts ───────────────────────────────────────────────────────────────────

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default()
}

/// Runs on a worker thread.
fn gather() -> Facts {
    let sysctl = |name: &str| output("sysctl", &["-n", name]);
    let gpu = output("pciconf", &["-lv"])
        .split("\nvgapci")
        .chain(std::iter::once(""))
        .find_map(|block| {
            if !block.contains("class      = display") {
                return None;
            }
            block.lines().find_map(|l| l.trim().strip_prefix("device").map(|v| v.trim_start_matches([' ', '=']).trim_matches('\'').to_owned()))
        })
        .unwrap_or_default();
    let booted = sysctl("kern.boottime").split("sec = ").nth(1).and_then(|r| r.split(',').next()).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let count = |args: &[&str]| output("pkg", args).lines().count() as u32;
    Facts {
        user: glib::user_name().to_string_lossy().into_owned(),
        host: glib::host_name().split('.').next().unwrap_or_default().to_owned(),
        os: format!("{} {}", output("uname", &["-s"]), output("freebsd-version", &["-u"])),
        kernel: output("uname", &["-i"]),
        arch: output("uname", &["-m"]),
        cpu: sysctl("hw.model"),
        cores: sysctl("hw.ncpu").parse().unwrap_or(0),
        gpu,
        screen: serde_json::from_str::<serde_json::Value>(&output("hyprctl", &["monitors", "-j"]))
            .ok()
            .and_then(|m| Some(format!("{}×{}", m[0]["width"].as_u64()?, m[0]["height"].as_u64()?)))
            .unwrap_or_default(),
        memory: sysctl("hw.physmem").parse().unwrap_or(0.0),
        desktop: output("hyprctl", &["version"]).lines().next().and_then(|l| l.split(" built").next()).unwrap_or("Hyprland").to_owned(),
        shell: std::env::var("SHELL").ok().and_then(|s| s.rsplit('/').next().map(str::to_owned)).unwrap_or_default(),
        packages: count(&["query", "%n"]),
        yours: count(&["query", "-e", "%a = 0", "%n"]),
        booted,
    }
}

fn gib(bytes: f64) -> String {
    format!("{:.1} GiB", bytes / 1024f64.powi(3))
}

/// "AMD Ryzen AI 9 365 w/ Radeon 880M" -> "Ryzen AI 9 365".
fn short_cpu(model: &str) -> String {
    let model = model.split(" w/ ").next().unwrap_or(model).split(" with ").next().unwrap_or(model);
    let model = model.replace("(R)", "").replace("(TM)", "").replace(" CPU", "");
    let model = model.trim_start_matches("AMD ").trim_start_matches("Intel ").trim_start_matches("Core ");
    model.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "Strix [Radeon 880M / 890M]" -> "Radeon 880M / 890M".
fn short_gpu(device: &str) -> String {
    match (device.find('['), device.rfind(']')) {
        (Some(a), Some(b)) if b > a => device[a + 1..b].to_owned(),
        _ => device.to_owned(),
    }
}

fn uptime(booted: i64) -> String {
    let now = glib::real_time() / 1_000_000;
    let mins = ((now - booted).max(0)) / 60;
    let (d, h, m) = (mins / 1440, mins / 60 % 24, mins % 60);
    if d > 0 { format!("{d}d {h}h") } else if h > 0 { format!("{h}h {m}m") } else { format!("{m}m") }
}
