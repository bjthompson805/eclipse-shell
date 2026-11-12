//! Adjust: brightness, contrast, saturation, warmth and a vignette for a
//! library picture, previewed live and saved as a new copy beside it (the
//! original stays). One ImageMagick pipeline makes both the small preview
//! and the full-size copy, so the preview is what gets written.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gio, glib, pango};
use gtk4 as gtk;

use super::{tune, ui, Walls};
use crate::theme::{self, TextStyle, HAIR};
use crate::wallpaper;

#[derive(Clone, Copy, Default, PartialEq)]
struct Grade {
    brightness: f64,
    contrast: f64,
    saturation: f64,
    warmth: f64,
    vignette: bool,
}

impl Grade {
    /// ImageMagick operators for this grade.
    fn args(self) -> Vec<String> {
        let mut args = Vec::new();
        if self.warmth != 0.0 {
            // Warm: more red, less blue; cool the other way.
            let k = self.warmth * 0.0018;
            args.extend(["-channel", "R", "-evaluate", "multiply", &format!("{:.4}", 1.0 + k), "+channel"].map(String::from));
            args.extend(["-channel", "B", "-evaluate", "multiply", &format!("{:.4}", 1.0 - k), "+channel"].map(String::from));
        }
        args.push("-modulate".into());
        args.push(format!("{},{},100", 100.0 + self.brightness, 100.0 + self.saturation));
        if self.contrast != 0.0 {
            args.push("-brightness-contrast".into());
            args.push(format!("0x{}", self.contrast));
        }
        if self.vignette {
            args.extend(["-background", "black", "-vignette", "0x18"].map(String::from));
        }
        args
    }
}

struct Adjust {
    walls: Rc<Walls>,
    path: PathBuf,
    grade: Cell<Grade>,
    picture: RefCell<Option<cairo::ImageSurface>>,
    preview: gtk::DrawingArea,
    generation: Cell<u32>,
    saving: Cell<bool>,
}

pub fn show(walls: &Rc<Walls>, path: &Path) {
    let s = walls.s;
    let px = |v: f64| (v * s).round() as i32;
    let (pw, ph) = walls.preview_size();
    let adjust = Rc::new(Adjust {
        walls: walls.clone(),
        path: path.to_owned(),
        grade: Cell::new(Grade::default()),
        picture: RefCell::default(),
        preview: gtk::DrawingArea::builder().content_width(pw).content_height(ph).build(),
        generation: Cell::new(0),
        saving: Cell::new(false),
    });
    let side = &walls.side;
    side.append(&tune::header(walls, "Adjust", path));
    adjust.preview.set_draw_func({
        let adjust = Rc::downgrade(&adjust);
        move |_, cr, w, h| {
            if let Some(a) = adjust.upgrade() {
                draw(cr, f64::from(w), f64::from(h), s, a.picture.borrow().as_ref());
            }
        }
    });
    side.append(&adjust.preview);

    let controls = gtk::Box::new(gtk::Orientation::Vertical, px(10.0));
    fn signed_fmt(v: f64) -> String {
        if v > 0.0 { format!("+{v:.0}") } else { format!("{v:.0}") }
    }
    let field = |name: &str, min: f64, max: f64, set: fn(&mut Grade, f64)| {
        let adjust = Rc::downgrade(&adjust);
        ui::slider(s, name, min, max, 0.0, signed_fmt, move |v| {
            if let Some(a) = adjust.upgrade() {
                let mut g = a.grade.get();
                set(&mut g, v);
                a.grade.set(g);
                a.render();
            }
        })
    };
    controls.append(&field("Brightness", -50.0, 50.0, |g, v| g.brightness = v));
    controls.append(&field("Contrast", -50.0, 50.0, |g, v| g.contrast = v));
    controls.append(&field("Saturation", -100.0, 100.0, |g, v| g.saturation = v));
    controls.append(&field("Warmth", -100.0, 100.0, |g, v| g.warmth = v));
    let vignette = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
    let label = TextStyle::new(10.0 * s, pango::Weight::Medium, theme::SUBTLE).label("Vignette");
    label.set_hexpand(true);
    label.set_xalign(0.0);
    vignette.append(&label);
    let switch_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    vignette.append(&switch_slot);
    fn fill_switch(adjust: &Rc<Adjust>, slot: &gtk::Box) {
        while let Some(c) = slot.first_child() {
            slot.remove(&c);
        }
        let on = adjust.grade.get().vignette;
        let (weak, slot_weak) = (Rc::downgrade(adjust), slot.downgrade());
        slot.append(&crate::popout::switch(adjust.walls.s, on, true, move || {
            if let (Some(a), Some(slot)) = (weak.upgrade(), slot_weak.upgrade()) {
                let mut g = a.grade.get();
                g.vignette = !on;
                a.grade.set(g);
                fill_switch(&a, &slot);
                a.render();
            }
        }));
    }
    fill_switch(&adjust, &switch_slot);
    controls.append(&vignette);
    side.append(&controls);

    let note = TextStyle::new(9.5 * s, pango::Weight::Medium, theme::FAINT).label("Saves a new copy beside the original.");
    note.set_xalign(0.0);
    side.append(&note);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
    buttons.append(&ui::button(s, Some("save"), "Save copy", true, true, {
        let adjust = Rc::downgrade(&adjust);
        move || adjust.upgrade().into_iter().for_each(|a| a.save(false))
    }));
    buttons.append(&ui::button(s, Some("wallpaper"), "Save and set", false, true, {
        let adjust = Rc::downgrade(&adjust);
        move || adjust.upgrade().into_iter().for_each(|a| a.save(true))
    }));
    side.append(&buttons);
    adjust.render();
    // Held by the window while it shows; everything else holds it weakly.
    *walls.tool.borrow_mut() = Some(adjust.clone());
}

fn scratch() -> PathBuf {
    glib::user_cache_dir().join("eclipse-shell").join("walls").join("adjust")
}

impl Adjust {
    /// Re-renders the preview a moment after the last change.
    fn render(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let adjust = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            let Some(a) = adjust.upgrade().filter(|a| a.generation.get() == generation) else { return };
            let (path, grade) = (a.path.clone(), a.grade.get());
            let size = a.preview.width().max(a.walls.preview_size().0) * a.walls.window.scale_factor().max(1);
            let adjust = Rc::downgrade(&a);
            glib::spawn_future_local(async move {
                let made = gio::spawn_blocking(move || {
                    let dir = scratch();
                    std::fs::create_dir_all(&dir).ok()?;
                    let out = dir.join(format!("preview-{generation}.png"));
                    let ok = Command::new("magick").arg(&path).args(["-resize", &format!("{size}x{size}>")]).args(grade.args()).arg(&out).status().ok()?.success();
                    let decoded = ok.then(|| wallpaper::decode_scaled(out.to_str()?, size)).flatten();
                    let _ = std::fs::remove_file(&out);
                    decoded
                })
                .await
                .ok()
                .flatten();
                if let (Some(a), Some((data, w, h))) = (adjust.upgrade().filter(|a| a.generation.get() == generation), made) {
                    *a.picture.borrow_mut() = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, w * 4).ok();
                    a.preview.queue_draw();
                }
            });
        });
    }

    fn save(self: &Rc<Self>, then_set: bool) {
        if self.saving.replace(true) {
            return;
        }
        let (path, grade) = (self.path.clone(), self.grade.get());
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "wallpaper".into());
        let dir = path.parent().map(Path::to_owned).unwrap_or_else(wallpaper::dir);
        let out = (1..).map(|n| dir.join(if n == 1 { format!("{stem}-adjusted.png") } else { format!("{stem}-adjusted-{n}.png") })).find(|p| !p.exists()).unwrap_or_default();
        self.walls.set_status("Saving the adjusted copy…");
        let adjust = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let target = out.clone();
            let ok = gio::spawn_blocking(move || Command::new("magick").arg(&path).args(grade.args()).arg(&target).status().is_ok_and(|s| s.success())).await.unwrap_or(false);
            let Some(a) = adjust.upgrade() else { return };
            a.saving.set(false);
            if !ok {
                a.walls.set_status("Couldn't save the copy");
                return;
            }
            a.walls.set_status(&format!("Saved {}", out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            if then_set {
                a.walls.set_wallpaper(&out);
            }
            a.walls.back(Some(out));
        });
    }
}

fn draw(cr: &cairo::Context, w: f64, h: f64, s: f64, picture: Option<&cairo::ImageSurface>) {
    let r = 10.0 * s;
    let _ = cr.save();
    theme::rounded_rect(cr, 0.0, 0.0, w, h, r);
    cr.clip();
    theme::set_source(cr, theme::palette().recessed);
    let _ = cr.paint();
    if let Some(p) = picture {
        let (iw, ih) = (f64::from(p.width()), f64::from(p.height()));
        let k = (w / iw).max(h / ih);
        cr.translate((w - iw * k) / 2.0, (h - ih * k) / 2.0);
        cr.scale(k, k);
        let _ = cr.set_source_surface(p, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Good);
        let _ = cr.paint();
    }
    let _ = cr.restore();
    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, r);
    cr.set_line_width(1.0);
    theme::set_source(cr, HAIR);
    let _ = cr.stroke();
}
