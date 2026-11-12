//! Window popout, dropping from the bar's title: the focused window's title
//! and icon, where and how it sits, and its process's CPU, memory and
//! command line, sampled every two seconds while open.
//!
//! The window is read from Hyprland before the popout opens; once the
//! popout holds the keyboard, Hyprland no longer has a focused window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;
use serde::Deserialize;

use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, DIM, HAIR, SUBTLE};
use crate::{hyprland, icons};

const NAMESPACE: &str = "eclipse-shell-window";
const SAMPLE: Duration = Duration::from_secs(2);

#[derive(Clone, Default, Deserialize)]
struct Active {
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pid: i32,
    #[serde(default)]
    size: [i32; 2],
    #[serde(default)]
    floating: bool,
    #[serde(default)]
    fullscreen: i32,
    #[serde(default)]
    xwayland: bool,
    #[serde(default)]
    monitor: i32,
    workspace: Option<Workspace>,
}

#[derive(Clone, Default, Deserialize)]
struct Workspace {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct Monitor {
    id: i32,
    name: String,
}

/// The process behind the window, from ps.
#[derive(Clone, Default)]
struct Process {
    cpu: String,
    rss_kb: f64,
    command: String,
}

pub struct WindowPopout {
    popout: Rc<Popout>,
    window: RefCell<Option<Active>>,
    monitor_name: RefCell<String>,
    /// Bumped on every open, so a stale sampler from an earlier open stops.
    generation: Cell<u32>,
}

impl WindowPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self { popout: Popout::new(app, NAMESPACE), window: RefCell::default(), monitor_name: RefCell::default(), generation: Cell::new(0) })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`, describing the window focused right now.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let active = hyprland::request("j/activewindow").ok().and_then(|j| serde_json::from_str::<Active>(&j).ok()).filter(|a| a.pid > 0);
        let monitors: Vec<Monitor> = hyprland::request("j/monitors").ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        *self.monitor_name.borrow_mut() = active.as_ref().and_then(|a| monitors.iter().find(|m| m.id == a.monitor)).map(|m| m.name.clone()).unwrap_or_default();
        *self.window.borrow_mut() = active;

        self.render(&Process::default());
        self.popout.show(Anchor::Under(x));

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let popout = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            loop {
                let Some(p) = popout.upgrade().filter(|p| p.popout.is_open() && p.generation.get() == generation) else { break };
                let pid = p.window.borrow().as_ref().map_or(0, |w| w.pid);
                drop(p);
                let process = sample(pid).await;
                let Some(p) = popout.upgrade().filter(|p| p.popout.is_open() && p.generation.get() == generation) else { break };
                p.render(&process);
                drop(p);
                glib::timeout_future(SAMPLE).await;
            }
        });
    }

    fn render(&self, process: &Process) {
        let s = self.popout.scale();
        let window = self.window.borrow();
        self.popout.set_content(&popout::card(&body(s, window.as_ref(), &self.monitor_name.borrow(), process)));
    }
}

fn body(s: f64, window: Option<&Active>, monitor: &str, process: &Process) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(px(11.0))
        .width_request(px(280.0) - 2 * px(16.0))
        .margin_top(px(13.0))
        .margin_bottom(px(14.0))
        .margin_start(px(16.0))
        .margin_end(px(16.0))
        .build();

    let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    header.append(&theme::filled_icon_glyph("select_window", 16.0 * s, theme::palette().accent));
    header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("WINDOW"));
    column.append(&header);

    let Some(w) = window else {
        let none = TextStyle::new(11.0 * s, pango::Weight::Normal, DIM).label("No focused window");
        none.set_xalign(0.0);
        column.append(&none);
        return column;
    };

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    let icon = gtk::Image::from_gicon(&icons::for_class(&w.class));
    icon.set_pixel_size(px(16.0));
    icon.set_valign(gtk::Align::Start);
    icon.set_margin_top(px(2.0));
    heading.append(&icon);
    let title = TextStyle::new(12.5 * s, pango::Weight::Semibold, CREAM).label(&w.title);
    title.set_wrap(true);
    title.set_wrap_mode(pango::WrapMode::WordChar);
    title.set_lines(2);
    title.set_ellipsize(pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_max_width_chars(1);
    heading.append(&title);
    column.append(&heading);

    column.append(&divider());
    let state = format!("{}{}", if w.floating { "Floating" } else { "Tiled" }, if w.fullscreen > 0 { " \u{b7} Fullscreen" } else { "" });
    let size = if w.size[0] > 0 { format!("{} \u{d7} {}", w.size[0], w.size[1]) } else { String::new() };
    let workspace = w.workspace.as_ref().map(|ws| ws.name.clone()).unwrap_or_default();
    let rows = gtk::Box::new(gtk::Orientation::Vertical, px(11.0));
    for (label, value) in [
        ("Class", w.class.clone()),
        ("Workspace", workspace),
        ("Monitor", monitor.to_owned()),
        ("Size", size),
        ("State", state),
        ("Xwayland", if w.xwayland { "Yes".into() } else { String::new() }),
    ] {
        if !value.is_empty() {
            rows.append(&info_row(s, label, &value));
        }
    }
    column.append(&rows);

    column.append(&divider());
    let header = TextStyle::new(9.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.4 * s).label("PROCESS");
    header.set_xalign(0.0);
    column.append(&header);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, px(11.0));
    rows.append(&info_row(s, "PID", &w.pid.to_string()));
    if !process.cpu.is_empty() {
        rows.append(&info_row(s, "CPU", &format!("{}%", process.cpu)));
    }
    if process.rss_kb > 0.0 {
        let mb = process.rss_kb / 1024.0;
        rows.append(&info_row(s, "Memory", &if mb < 10.0 { format!("{mb:.1} MB") } else { format!("{} MB", mb.round()) }));
    }
    column.append(&rows);
    if !process.command.is_empty() {
        let command = gtk::Box::new(gtk::Orientation::Vertical, px(3.0));
        let label = TextStyle::new(9.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.2 * s).label("COMMAND");
        label.set_xalign(0.0);
        command.append(&label);
        let value = TextStyle::new(9.5 * s, pango::Weight::Normal, DIM).mono().label(&process.command);
        value.set_wrap(true);
        value.set_wrap_mode(pango::WrapMode::WordChar);
        value.set_xalign(0.0);
        value.set_max_width_chars(1);
        value.set_hexpand(true);
        command.append(&value);
        column.append(&command);
    }
    column
}

fn info_row(s: f64, label: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (10.0 * s).round() as i32);
    let name = TextStyle::new(10.5 * s, pango::Weight::Medium, SUBTLE).label(label);
    name.set_valign(gtk::Align::Baseline);
    row.append(&name);
    let value = TextStyle::new(10.5 * s, pango::Weight::Semibold, CREAM).label(value);
    value.set_hexpand(true);
    value.set_xalign(1.0);
    value.set_ellipsize(pango::EllipsizeMode::End);
    value.set_valign(gtk::Align::Baseline);
    row.append(&value);
    row
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

/// CPU share, resident memory and command line of `pid`.
async fn sample(pid: i32) -> Process {
    if pid <= 0 {
        return Process::default();
    }
    let pid = pid.to_string();
    let argv = ["ps", "-o", "pcpu=", "-o", "rss=", "-o", "args=", "-p", &pid].map(std::ffi::OsStr::new);
    let Ok(child) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) else { return Process::default() };
    let Ok((Some(out), _)) = child.communicate_utf8_future(None).await else { return Process::default() };
    // One "-o" per column: FreeBSD reads anything after "pcpu=" as its header.
    // "  0.3  51234 kitty --single-instance": the command is the rest of the line.
    let line = out.trim_start();
    let Some((cpu, rest)) = line.split_once(char::is_whitespace) else { return Process::default() };
    let Some((rss, command)) = rest.trim_start().split_once(char::is_whitespace) else { return Process::default() };
    Process { cpu: cpu.to_owned(), rss_kb: rss.parse().unwrap_or(0.0), command: command.trim().to_owned() }
}
