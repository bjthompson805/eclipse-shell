//! The screen recorder: wf-recorder, hardware-encoded through VAAPI with a
//! CPU fallback, into ~/Videos/Recordings. A port of Ryoku's recorder.
//!
//! - [`cli`]: `eclipse-shell record ...`, which starts and stops wf-recorder
//!   (run in its own process, so a recording outlives whoever asked for it).
//! - The state here: whether a recording runs, reconciled against the live
//!   process, its elapsed time, and the chooser's choices.
//! - [`hud`]: the island that melts out of the frame edge: the chooser
//!   before a recording, the timer and stop during one.
//! - [`deck`]: the right sidebar's Record tab: record or stop, and every
//!   recording.
//! - [`app`]: Eclipse Recorder, the settings and every recording.

pub mod app;
pub mod cli;
pub mod deck;
mod hud;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;
use serde::{Deserialize, Serialize};

use crate::theme::{self, Rgba, TextStyle, CREAM};

/// The recording red, for the dot, the REC tag and stop.
pub const RED: Rgba = Rgba::rgb8(0xe2, 0x4b, 0x3f);

// ---- settings ----------------------------------------------------------------

/// What every recording uses; Eclipse Recorder edits it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fps: u32,
    /// medium, high, very_high or ultra.
    pub quality: String,
    /// h264, hevc or av1.
    pub codec: String,
    /// gpu (VAAPI) or cpu.
    pub encoder: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { fps: 60, quality: "very_high".into(), codec: "h264".into(), encoder: "gpu".into() }
    }
}

fn settings_path() -> PathBuf {
    glib::user_config_dir().join("eclipse-shell").join("recording.json")
}

/// The saved settings; ECLIPSE_SCREENRECORD_FPS, _QUALITY, _CODEC and
/// _ENCODER override them.
pub fn settings() -> Settings {
    let mut s: Settings = std::fs::read_to_string(settings_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let env = |k: &str| std::env::var(format!("ECLIPSE_SCREENRECORD_{k}")).ok().filter(|v| !v.is_empty());
    if let Some(v) = env("FPS").and_then(|v| v.parse().ok()) {
        s.fps = v;
    }
    if let Some(v) = env("QUALITY") {
        s.quality = v;
    }
    if let Some(v) = env("CODEC") {
        s.codec = v;
    }
    if let Some(v) = env("ENCODER") {
        s.encoder = v;
    }
    s
}

pub fn save_settings(s: &Settings) {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(s).unwrap_or_default() + "\n");
}

// ---- recordings --------------------------------------------------------------

pub fn recordings_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Videos).unwrap_or_else(|| glib::home_dir().join("Videos")).join("Recordings")
}

pub struct Recording {
    pub path: PathBuf,
    /// "Nov 24, 16:20", from the file name.
    pub label: String,
    pub size: String,
}

/// Every recording, newest first.
pub fn recordings() -> Vec<Recording> {
    let mut found: Vec<(std::time::SystemTime, PathBuf, u64)> = std::fs::read_dir(recordings_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("recording_") && n.ends_with(".mp4")))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), e.path(), meta.len()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path, len)| Recording { label: pretty_name(&path), size: human_size(len), path }).collect()
}

/// "recording_2026-11-24_16.20.05" as "Nov 24, 16:20".
fn pretty_name(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let parse = || -> Option<String> {
        let rest = stem.strip_prefix("recording_")?;
        let (date, time) = rest.split_once('_')?;
        let d: Vec<i32> = date.split('-').map(|p| p.parse().ok()).collect::<Option<_>>()?;
        let t: Vec<i32> = time.get(..8)?.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
        let (&[y, mo, da], &[h, mi, se]) = (d.as_slice(), t.as_slice()) else { return None };
        let dt = glib::DateTime::from_local(y, mo, da, h, mi, f64::from(se)).ok()?;
        dt.format("%b %-d, %H:%M").ok().map(|s| s.to_string())
    };
    parse().unwrap_or(stem)
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{} KB", kb.round());
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return if mb < 10.0 { format!("{mb:.1} MB") } else { format!("{} MB", mb.round()) };
    }
    format!("{:.1} GB", mb / 1024.0)
}

/// Opens a recording, or the folder, in the default app.
pub fn open(path: &Path) {
    crate::audio::spawn(&["xdg-open", &path.display().to_string()]);
}

/// Moves a recording to the trash, or deletes it where there's none.
pub fn trash(path: &Path) {
    if gio::File::for_path(path).trash(gio::Cancellable::NONE).is_err() {
        let _ = std::fs::remove_file(path);
    }
}

/// "m:ss", or "h:mm:ss" past an hour.
pub fn elapsed_text(sec: u64) -> String {
    let (h, m, s) = (sec / 3600, (sec % 3600) / 60, sec % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// ---- state -------------------------------------------------------------------

/// Whether a recording runs, and the chooser's choices. The bar module, the
/// deck and the HUD all follow this one state.
#[derive(Default)]
struct State {
    active: bool,
    started: Option<Instant>,
    /// Region capture: the box picked as "WxH+X+Y" (logical), or none for
    /// the whole monitor.
    region: Option<String>,
    chooser_open: bool,
    /// Between "Record" and the recorder starting, so the chooser is gone
    /// before the first frame.
    starting: bool,
    desktop_audio: bool,
    mic: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
    static LISTENERS: RefCell<Vec<Rc<dyn Fn()>>> = RefCell::default();
    static HUD: RefCell<Option<Rc<hud::Hud>>> = const { RefCell::new(None) };
    /// Bumped by every poll scheduled, so only the newest one reschedules.
    static POLLS: Cell<u32> = const { Cell::new(0) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with_borrow_mut(f)
}

/// Runs `f` on every change, and once a second while recording.
pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with_borrow_mut(|l| l.push(Rc::new(f)));
}

fn changed() {
    let all: Vec<Rc<dyn Fn()>> = LISTENERS.with_borrow(|l| l.clone());
    for f in all {
        f();
    }
}

pub fn active() -> bool {
    with(|s| s.active)
}

pub fn elapsed() -> u64 {
    with(|s| s.started.map_or(0, |t| t.elapsed().as_secs()))
}

pub fn chooser_open() -> bool {
    with(|s| s.chooser_open)
}

fn starting() -> bool {
    with(|s| s.starting)
}

fn region() -> Option<String> {
    with(|s| s.region.clone())
}

fn audio_choices() -> (bool, bool) {
    with(|s| (s.desktop_audio, s.mic))
}

fn set(f: impl FnOnce(&mut State)) {
    with(f);
    changed();
}

/// Sets up the island and starts following wf-recorder. Called once, as the
/// shell starts.
pub fn start(app: &gtk::Application) {
    let hud = hud::Hud::new(app);
    HUD.with_borrow_mut(|h| *h = Some(hud));
    // The clock: once a second while recording.
    glib::timeout_add_seconds_local(1, || {
        if active() {
            changed();
        }
        glib::ControlFlow::Continue
    });
    poll_after(Duration::ZERO);
}

pub fn set_monitor(monitor: Option<gdk::Monitor>) {
    HUD.with_borrow(|h| {
        if let Some(hud) = h {
            hud.set_monitor(monitor);
        }
    });
}

/// Melts the island's chooser out of the frame, the step before a recording.
pub fn open_chooser() {
    if active() {
        return;
    }
    set(|s| s.chooser_open = true);
}

/// `record-chooser`: opens the chooser, or melts an open one back in.
pub fn toggle_chooser() {
    if chooser_open() { close_chooser() } else { open_chooser() }
}

pub fn close_chooser() {
    set(|s| s.chooser_open = false);
}

fn toggle_desktop_audio() {
    set(|s| s.desktop_audio = !s.desktop_audio);
}

fn toggle_mic() {
    set(|s| s.mic = !s.mic);
}

/// Picks a region with slurp, or with one picked, goes back to the whole
/// monitor.
fn toggle_region() {
    if region().is_some() {
        return set(|s| s.region = None);
    }
    let (done, picked) = async_channel::bounded::<Option<String>>(1);
    std::thread::spawn(move || {
        let out = std::process::Command::new("slurp").args(["-f", "%wx%h+%x+%y"]).stderr(std::process::Stdio::null()).output();
        let g = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        let _ = done.send_blocking(g.filter(|g| valid_geometry(g)));
    });
    glib::spawn_future_local(async move {
        if let Ok(g) = picked.recv().await {
            set(|s| s.region = g);
        }
    });
}

/// "WxH+X+Y" in whole numbers.
pub fn valid_geometry(g: &str) -> bool {
    let Some((size, pos)) = g.split_once('+') else { return false };
    let Some((w, h)) = size.split_once('x') else { return false };
    let Some((x, y)) = pos.split_once('+') else { return false };
    [w, h, x, y].iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The chooser's "Record": records the region, or after a moment for the
/// chooser to leave the screen, the monitor.
fn start_quick() {
    let (desktop, mic) = audio_choices();
    let mut args: Vec<String> = Vec::new();
    if desktop {
        args.push("--desktop-audio".into());
    }
    if mic {
        args.push("--mic".into());
    }
    if let Some(g) = region() {
        args.splice(0..0, ["--region".to_owned(), g]);
        set(|s| s.chooser_open = false);
        return begin(args);
    }
    set(|s| {
        s.chooser_open = false;
        s.starting = true;
    });
    glib::timeout_add_local_once(Duration::from_millis(420), move || {
        with(|s| s.starting = false);
        begin(args);
    });
}

/// Starts `eclipse-shell record args`, counting from now; a poll shortly
/// after confirms it came up.
fn begin(args: Vec<String>) {
    spawn_self(&std::iter::once("record".to_owned()).chain(args).collect::<Vec<_>>());
    set(|s| {
        s.active = true;
        s.started = Some(Instant::now());
    });
    poll_after(Duration::from_millis(2500));
}

pub fn stop() {
    spawn_self(&["record".to_owned(), "stop".to_owned()]);
    set(|s| {
        s.active = false;
        s.started = None;
    });
}

/// Runs this binary with `args` in a process group of its own, reaped in the
/// background.
fn spawn_self(args: &[String]) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    if let Ok(mut child) = std::process::Command::new(exe).args(args).process_group(0).stdin(std::process::Stdio::null()).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Reconciles with the live process after `delay`, then keeps doing so: every
/// 2 s while recording, so an outside stop or a crash can't strand the
/// controls, and every 30 s otherwise, to catch one started elsewhere.
fn poll_after(delay: Duration) {
    let generation = POLLS.with(|p| {
        p.set(p.get() + 1);
        p.get()
    });
    glib::timeout_add_local_once(delay, move || {
        if POLLS.with(Cell::get) != generation {
            return;
        }
        glib::spawn_future_local(async move {
            let running = gio::spawn_blocking(cli::running).await.unwrap_or(false);
            let was = active();
            if running != was || (running && with(|s| s.started.is_none())) {
                set(|s| {
                    s.active = running;
                    s.started = running.then(|| s.started.unwrap_or_else(Instant::now));
                });
            }
            if POLLS.with(Cell::get) == generation {
                poll_after(Duration::from_secs(if running { 2 } else { 30 }));
            }
        });
    });
}

// ---- shared widgets ----------------------------------------------------------

/// A dot that pulses while recording (opacity 1 to 0.18 and back, 1.24 s).
fn pulse_dot(px: f64, color: Rgba) -> gtk::DrawingArea {
    let size = px.round() as i32;
    let dot = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).halign(gtk::Align::Center).build();
    dot.set_draw_func(move |area, cr, w, h| {
        let t = area.frame_clock().map_or(0.0, |c| c.frame_time() as f64 / 1e6);
        let wave = 0.5 + 0.5 * (t * std::f64::consts::TAU / 1.24).cos();
        let alpha = if active() { 0.18 + 0.82 * wave } else { 1.0 };
        let (w, h) = (f64::from(w), f64::from(h));
        cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
        theme::set_source(cr, color.alpha(alpha));
        let _ = cr.fill();
    });
    dot.add_tick_callback(|area, _| {
        if active() {
            area.queue_draw();
        }
        glib::ControlFlow::Continue
    });
    dot
}

/// A flat icon button for the HUD and the deck: the tint carries the state,
/// hover lights the tile.
fn icon_button(s: f64, glyph: &str, tint: Rgba, hint: &str, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let size = (26.0 * s).round() as i32;
    let hovered = Rc::new(Cell::new(false));
    let back = gtk::DrawingArea::builder().content_width(size).content_height(size).build();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            if hovered.get() {
                theme::rounded_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), 7.0 * s);
                theme::set_source(cr, theme::palette().raised);
                let _ = cr.fill();
            }
        }
    });
    let icon = theme::icon_glyph(glyph, 15.0 * s, tint);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    let button = gtk::Overlay::new();
    button.set_child(Some(&back));
    button.add_overlay(&icon);
    button.set_valign(gtk::Align::Center);
    button.set_cursor_from_name(Some("pointer"));
    // Shown at once, on the window's hint layer.
    crate::hint::float_label(&button, s, hint);
    button.update_property(&[gtk::accessible::Property::Label(hint)]);
    crate::popout::on_hover(&button, {
        let back = back.downgrade();
        move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
        }
    });
    crate::popout::on_click(&button, pressed);
    button
}

/// The elapsed time, in tabular figures.
fn clock_label(px: f64) -> gtk::Label {
    let label = TextStyle::new(px, pango::Weight::Medium, CREAM).label(&elapsed_text(elapsed()));
    let attrs = label.attributes().unwrap_or_default();
    attrs.insert(pango::AttrFontFeatures::new("tnum 1"));
    label.set_attributes(Some(&attrs));
    label
}
