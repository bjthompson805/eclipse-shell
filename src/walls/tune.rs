//! Tune colours: wallust options for one wallpaper, previewed before they
//! touch the desktop. wallust runs in a scratch cache with templates and
//! terminal sequences off and prints the scheme; the preview shows the 16
//! colours and the shell's frame and bar drawn in them over the picture.
//! Saving stores the options for that file (the shell applies them whenever
//! it's set) and sets it.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gio, glib, pango};
use gtk4 as gtk;

use super::{ui, Walls};
use crate::theme::{self, Palette, Rgba, TextStyle, BRIGHT, DIM, HAIR};
use crate::wallpaper;

const PALETTES: &[&str] = &["dark16", "harddark16", "saliencedark16", "saliencedarkbalanced16", "softdark16", "darkcomp16", "light16", "softlight16"];
const COLORSPACES: &[&str] = &["lch", "lab", "salience", "lchmixed", "labmixed"];
const BACKENDS: &[&str] = &["fastresize", "resized", "kmeans", "full", "wal"];

#[derive(Clone, Default)]
struct Options {
    palette: Option<String>,
    colorspace: Option<String>,
    backend: Option<String>,
    /// 0: wallust's own.
    saturation: i64,
    threshold: i64,
    contrast: bool,
}

impl Options {
    fn from_json(t: &serde_json::Value) -> Self {
        let text = |k: &str| t[k].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
        Self { palette: text("palette"), colorspace: text("colorspace"), backend: text("backend"), saturation: t["saturation"].as_i64().unwrap_or(0), threshold: t["threshold"].as_i64().unwrap_or(0), contrast: t["contrast"].as_bool().unwrap_or(false) }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "palette": self.palette.clone().unwrap_or_default(),
            "colorspace": self.colorspace.clone().unwrap_or_default(),
            "backend": self.backend.clone().unwrap_or_default(),
            "saturation": self.saturation,
            "threshold": self.threshold,
            "contrast": self.contrast,
        })
    }
}

struct Tune {
    walls: Rc<Walls>,
    path: PathBuf,
    options: RefCell<Options>,
    /// The 16 colours for the options as they stand.
    scheme: RefCell<Vec<Rgba>>,
    picture: RefCell<Option<cairo::ImageSurface>>,
    preview: gtk::DrawingArea,
    strip: gtk::DrawingArea,
    controls: gtk::Box,
    generation: Cell<u32>,
}

pub fn show(walls: &Rc<Walls>, path: &Path) {
    let s = walls.s;
    let px = |v: f64| (v * s).round() as i32;
    let saved = wallpaper::tunes().get(&path.display().to_string()).cloned();
    let (pw, ph) = walls.preview_size();
    let tune = Rc::new(Tune {
        walls: walls.clone(),
        path: path.to_owned(),
        options: RefCell::new(saved.as_ref().map(Options::from_json).unwrap_or_default()),
        scheme: RefCell::default(),
        picture: RefCell::default(),
        preview: gtk::DrawingArea::builder().content_width(pw).content_height(ph).build(),
        strip: gtk::DrawingArea::builder().content_height(px(20.0)).hexpand(true).build(),
        controls: gtk::Box::new(gtk::Orientation::Vertical, px(12.0)),
        generation: Cell::new(0),
    });

    let side = &walls.side;
    side.append(&header(walls, "Tune colours", path));
    tune.preview.set_draw_func({
        let tune = Rc::downgrade(&tune);
        move |_, cr, w, h| {
            if let Some(t) = tune.upgrade() {
                draw_mock(cr, f64::from(w), f64::from(h), s, t.picture.borrow().as_ref(), &t.scheme.borrow());
            }
        }
    });
    side.append(&tune.preview);
    tune.strip.set_draw_func({
        let tune = Rc::downgrade(&tune);
        move |_, cr, w, h| {
            if let Some(t) = tune.upgrade() {
                draw_strip(cr, f64::from(w), f64::from(h), s, &t.scheme.borrow());
            }
        }
    });
    side.append(&tune.strip);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&tune.controls).vexpand(true).build();
    side.append(&scroller);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    buttons.append(&ui::button(s, Some("check"), "Save and set", true, true, {
        let tune = Rc::downgrade(&tune);
        move || {
            if let Some(t) = tune.upgrade() {
                wallpaper::save_tune(&t.path, Some(t.options.borrow().to_json()));
                t.walls.set_wallpaper(&t.path);
                t.walls.back(None);
            }
        }
    }));
    buttons.append(&ui::button(s, Some("restart_alt"), "Defaults", false, true, {
        let tune = Rc::downgrade(&tune);
        move || {
            if let Some(t) = tune.upgrade() {
                wallpaper::save_tune(&t.path, None);
                *t.options.borrow_mut() = Options::default();
                t.render_controls();
                t.recompute();
            }
        }
    }));
    side.append(&buttons);

    tune.render_controls();
    // Held by the window while it shows; everything else holds it weakly.
    *walls.tool.borrow_mut() = Some(tune.clone());
    tune.recompute();
    // The picture: the file, or a clip's first frame.
    let (file, height) = (path.to_owned(), ph * walls.window.scale_factor().max(1));
    let weak = Rc::downgrade(&tune);
    glib::spawn_future_local(async move {
        let decoded = gio::spawn_blocking(move || wallpaper::still(&file).and_then(|f| wallpaper::decode_scaled(f.to_str()?, height))).await.ok().flatten();
        let (Some(t), Some((data, w, h))) = (weak.upgrade(), decoded) else { return };
        *t.picture.borrow_mut() = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, w * 4).ok();
        t.preview.queue_draw();
    });
}

/// A back arrow, a title and the file name, for a tool's column.
pub(super) fn header(walls: &Rc<Walls>, title: &str, path: &Path) -> gtk::Box {
    let s = walls.s;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (10.0 * s).round() as i32);
    let back = theme::icon_glyph("arrow_back", 18.0 * s, theme::ICON_DIM);
    let weak = Rc::downgrade(walls);
    crate::popout::on_click(&back, move || {
        if let Some(w) = weak.upgrade() {
            w.back(None);
        }
    });
    row.append(&back);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, (2.0 * s).round() as i32);
    let t = TextStyle::new(13.0 * s, pango::Weight::Semibold, BRIGHT).label(title);
    t.set_xalign(0.0);
    titles.append(&t);
    let name = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).mono().label(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    name.set_xalign(0.0);
    name.set_ellipsize(pango::EllipsizeMode::Middle);
    titles.append(&name);
    row.append(&titles);
    row
}

impl Tune {
    fn render_controls(self: &Rc<Self>) {
        let s = self.walls.s;
        while let Some(c) = self.controls.first_child() {
            self.controls.remove(&c);
        }
        let options = self.options.borrow().clone();
        let pick = |label: &str, list: &'static [&'static str], current: Option<String>, set: fn(&mut Options, Option<String>)| {
            let section = gtk::Box::new(gtk::Orientation::Vertical, (6.0 * s).round() as i32);
            section.append(&crate::popout::micro_label(s, label));
            let mut labels: Vec<&str> = vec!["auto"];
            labels.extend_from_slice(list);
            let chosen = current.as_deref().and_then(|c| list.iter().position(|x| *x == c)).map_or(0, |i| i + 1);
            let tune = Rc::downgrade(self);
            section.append(&ui::chips(s, &labels, Some(chosen), move |i| {
                if let Some(t) = tune.upgrade() {
                    set(&mut t.options.borrow_mut(), if i == 0 { None } else { Some(list[i - 1].to_owned()) });
                    t.render_controls();
                    t.recompute();
                }
            }));
            section
        };
        self.controls.append(&pick("Palette", PALETTES, options.palette.clone(), |o, v| o.palette = v));
        self.controls.append(&pick("Colour space", COLORSPACES, options.colorspace.clone(), |o, v| o.colorspace = v));
        self.controls.append(&pick("Backend", BACKENDS, options.backend.clone(), |o, v| o.backend = v));
        let tune = Rc::downgrade(self);
        self.controls.append(&ui::slider(s, "Saturation", 0.0, 100.0, options.saturation as f64, |v| if v == 0.0 { "auto".into() } else { format!("{v:.0}") }, move |v| {
            if let Some(t) = tune.upgrade() {
                t.options.borrow_mut().saturation = v as i64;
                t.recompute();
            }
        }));
        let tune = Rc::downgrade(self);
        self.controls.append(&ui::slider(s, "Threshold", 0.0, 100.0, options.threshold as f64, |v| if v == 0.0 { "auto".into() } else { format!("{v:.0}") }, move |v| {
            if let Some(t) = tune.upgrade() {
                t.options.borrow_mut().threshold = v as i64;
                t.recompute();
            }
        }));
        let contrast = gtk::Box::new(gtk::Orientation::Horizontal, (10.0 * s).round() as i32);
        let label = TextStyle::new(10.0 * s, pango::Weight::Medium, theme::SUBTLE).label("Keep text readable (contrast check)");
        label.set_hexpand(true);
        label.set_xalign(0.0);
        contrast.append(&label);
        let tune = Rc::downgrade(self);
        let on = options.contrast;
        contrast.append(&crate::popout::switch(s, on, true, move || {
            if let Some(t) = tune.upgrade() {
                t.options.borrow_mut().contrast = !on;
                t.render_controls();
                t.recompute();
            }
        }));
        self.controls.append(&contrast);
    }

    /// Re-runs wallust a moment after the last change.
    fn recompute(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let tune = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(t) = tune.upgrade().filter(|t| t.generation.get() == generation) else { return };
            let (path, args) = (t.path.clone(), wallpaper::tune_args(&t.options.borrow().to_json()));
            let tune = Rc::downgrade(&t);
            glib::spawn_future_local(async move {
                let scheme = gio::spawn_blocking(move || scheme(&path, &args)).await.ok().flatten();
                if let (Some(t), Some(scheme)) = (tune.upgrade().filter(|t| t.generation.get() == generation), scheme) {
                    *t.scheme.borrow_mut() = scheme;
                    t.preview.queue_draw();
                    t.strip.queue_draw();
                }
            });
        });
    }
}

/// The 16 colours wallust picks for `path` with `args`, without touching
/// the desktop. Runs on a worker thread.
fn scheme(path: &Path, args: &[String]) -> Option<Vec<Rgba>> {
    let still = wallpaper::still(path)?;
    let scratch = glib::user_cache_dir().join("eclipse-shell").join("walls").join("wallust-scratch");
    std::fs::create_dir_all(&scratch).ok()?;
    let out = Command::new("wallust").arg("run").arg(&still).args(["-T", "-s", "-q", "--no-hooks", "--print-scheme"]).args(args).env("XDG_CACHE_HOME", &scratch).output().ok()?;
    let colours: Vec<Rgba> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| Rgba::hex(l.trim())).collect();
    (colours.len() >= 8).then_some(colours)
}

fn draw_strip(cr: &cairo::Context, w: f64, h: f64, s: f64, scheme: &[Rgba]) {
    let _ = cr.save();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, 5.0 * s);
    cr.clip();
    theme::set_source(cr, theme::palette().recessed);
    let _ = cr.paint();
    let cell = w / scheme.len().max(1) as f64;
    for (i, c) in scheme.iter().enumerate() {
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

/// The picture with the shell's frame and bar drawn in the scheme's colours
/// over it, so a tune is seen as it would look.
fn draw_mock(cr: &cairo::Context, w: f64, h: f64, s: f64, picture: Option<&cairo::ImageSurface>, scheme: &[Rgba]) {
    let r = 10.0 * s;
    let _ = cr.save();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, r);
    cr.clip();
    theme::set_source(cr, theme::palette().recessed);
    let _ = cr.paint();
    if let Some(p) = picture {
        let (iw, ih) = (f64::from(p.width()), f64::from(p.height()));
        let k = (w / iw).max(h / ih);
        let _ = cr.save();
        cr.translate((w - iw * k) / 2.0, (h - ih * k) / 2.0);
        cr.scale(k, k);
        let _ = cr.set_source_surface(p, 0.0, 0.0);
        let _ = cr.paint();
        let _ = cr.restore();
    }
    if let (Some(&bg), Some(&c4)) = (scheme.first(), scheme.get(4)) {
        let palette = Palette::from_wallust(bg, c4);
        let (edge, band) = (4.0 * s, 16.0 * s);
        // The frame around a rounded hole.
        cr.set_fill_rule(cairo::FillRule::EvenOdd);
        cr.rectangle(0.0, 0.0, w, h);
        theme::rounded_rect(cr, edge, band, w - 2.0 * edge, h - band - edge, 5.0 * s);
        theme::set_source(cr, palette.base);
        let _ = cr.fill();
        theme::rounded_rect(cr, edge, band, w - 2.0 * edge, h - band - edge, 5.0 * s);
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.edge);
        let _ = cr.stroke();
        // Pills along the bar: workspaces, a clock, the status cluster.
        let pill = |cr: &cairo::Context, x: f64, width: f64, fill: Rgba| {
            theme::rounded_rect(cr, x, band * 0.2, width, band * 0.6, band * 0.3);
            theme::set_source(cr, fill);
            let _ = cr.fill();
        };
        pill(cr, 8.0 * s, 9.0 * s, palette.accent);
        pill(cr, 20.0 * s, 34.0 * s, palette.raised);
        pill(cr, 25.0 * s, 8.0 * s, palette.accent_lit);
        pill(cr, w / 2.0 - 22.0 * s, 44.0 * s, palette.raised);
        pill(cr, w - 70.0 * s, 46.0 * s, palette.raised);
        pill(cr, w - 20.0 * s, 12.0 * s, palette.accent.alpha(0.7));
    }
    let _ = cr.restore();
    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, r);
    cr.set_line_width(1.0);
    theme::set_source(cr, HAIR);
    let _ = cr.stroke();
}
