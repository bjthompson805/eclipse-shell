//! Eclipse Recorder, `eclipse-shell recorder`: the recorder's own app. The
//! settings behind every recording (framerate, quality, codec, encoder),
//! which encoder this machine will use, a Record button, and every
//! recording with play, folder and trash. Settings save as they change.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;

use super::{recordings, recordings_dir, settings, save_settings, Settings, RED};
use crate::theme::{self, TextStyle, BRIGHT, CREAM, DIM, FAINT};
use crate::walls::ui;
use crate::{app_window, config, hyprland, mark, popout};

const APP_ID: &str = "io.github.bjthompson805.EclipseRecorder";

const FPS: &[(&str, u32)] = &[("30 fps", 30), ("60 fps", 60), ("120 fps", 120)];
const QUALITY: &[(&str, &str)] = &[("Medium", "medium"), ("High", "high"), ("Very high", "very_high"), ("Ultra", "ultra")];
const CODEC: &[(&str, &str)] = &[("H.264", "h264"), ("HEVC", "hevc"), ("AV1", "av1")];
const ENCODER: &[(&str, &str)] = &[("GPU (VAAPI)", "gpu"), ("CPU", "cpu")];

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    let shown: Rc<RefCell<Option<Rc<Recorder>>>> = Rc::default();
    app.connect_activate(move |app| {
        if let Some(r) = shown.borrow().as_ref() {
            r.window.present();
            return;
        }
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(crate::TRANSPARENT_WINDOW);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        theme::watch_palette();
        theme::font_settings();
        *shown.borrow_mut() = Some(Recorder::new(app));
    });
    app.run_with_args::<&str>(&[])
}

struct Recorder {
    window: gtk::ApplicationWindow,
    s: f64,
    content: gtk::Box,
}

impl Recorder {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let monitor = gdk::Display::default().and_then(|d| d.monitors().item(0)).and_downcast::<gdk::Monitor>();
        let (sw, sh) = monitor.as_ref().map_or((1920, 1080), |m| (m.geometry().width(), m.geometry().height()));
        let s = config::bar_scale(sh);
        let (ww, wh) = ((f64::from(sw) * 0.42).min(720.0) as i32, (f64::from(sh) * 0.72).min(820.0) as i32);
        hyprland::eval(&format!("hl.window_rule({{ name = \"eclipse-recorder\", match = {{ class = \"^({APP_ID})$\" }}, float = true, size = {{ {ww}, {wh} }}, center = true }})"));
        let window = gtk::ApplicationWindow::builder().application(app).title("Recorder").default_width(ww).default_height(wh).decorated(false).css_classes(["eclipse-shell"]).build();
        let px = |v: f64| (v * s).round() as i32;

        let back = gtk::DrawingArea::new();
        back.set_draw_func(|_, cr, _, _| {
            theme::set_source(cr, theme::palette().base);
            let _ = cr.paint();
        });
        theme::on_palette_change({
            let back = back.downgrade();
            move || back.upgrade().into_iter().for_each(|b| b.queue_draw())
        });

        let body = gtk::Box::new(gtk::Orientation::Vertical, px(12.0));
        body.set_margin_start(px(20.0));
        body.set_margin_end(px(20.0));
        body.set_margin_top(px(18.0));
        body.set_margin_bottom(px(14.0));
        let top = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        top.append(&mark::mark(18.0 * s));
        let title = TextStyle::new(15.0 * s, pango::Weight::Bold, BRIGHT).label("Recorder");
        top.append(&title);
        let what = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label("Eclipse");
        what.set_valign(gtk::Align::Baseline);
        what.set_hexpand(true);
        what.set_xalign(0.0);
        top.append(&what);
        top.append(&app_window::close_button(s, &window));
        body.append(&top);

        let content = gtk::Box::new(gtk::Orientation::Vertical, px(10.0));
        content.set_margin_end(px(12.0));
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&content).build();
        body.append(&scroller);
        let foot = TextStyle::new(9.0 * s, pango::Weight::Medium, FAINT).mono().label("~/.config/eclipse-shell/recording.json");
        foot.set_xalign(0.0);
        body.append(&foot);

        let layers = gtk::Overlay::new();
        layers.set_child(Some(&back));
        layers.add_overlay(&body);
        // The buttons' hover hints float over everything.
        layers.add_overlay(&crate::hint::layer());
        window.set_child(Some(&layers));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let window = window.downgrade();
            move |_, key, _, modifiers| {
                if key == gdk::Key::Escape || (key == gdk::Key::q && modifiers.contains(gdk::ModifierType::CONTROL_MASK)) {
                    window.upgrade().into_iter().for_each(|w| w.close());
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        window.add_controller(keys);

        let recorder = Rc::new(Self { window, s, content });
        recorder.show();
        recorder.window.present();
        recorder
    }

    fn show(self: &Rc<Self>) {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        let current = settings();

        let intro = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label("Records with wf-recorder, hardware-encoded on your GPU through VAAPI; if the GPU encoder can't start it falls back to the CPU. Files land in ~/Videos/Recordings.");
        intro.set_xalign(0.0);
        intro.set_wrap(true);
        self.content.append(&intro);

        // Record, and the encoder this machine will use.
        let head = line(s);
        let words = gtk::Box::new(gtk::Orientation::Vertical, px(3.0));
        words.set_hexpand(true);
        words.set_valign(gtk::Align::Center);
        let label = TextStyle::new(11.5 * s, pango::Weight::Semibold, CREAM).label("Encoder");
        label.set_xalign(0.0);
        words.append(&label);
        let info = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).mono().label(&super::cli::encoder_info());
        info.set_xalign(0.0);
        info.set_wrap(true);
        words.append(&info);
        head.append(&words);
        let record = ui::button(s, Some("fiber_manual_record"), "Record", true, true, || {
            let Ok(exe) = crate::exe() else { return };
            crate::audio::spawn(&[&exe.display().to_string(), "record-chooser"]);
        });
        record.set_valign(gtk::Align::Center);
        head.append(&record);
        self.content.append(&card(&head));

        self.content.append(&popout::eyebrow(s, "Quality"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.append(&self.choice("Framerate", "Higher is smoother; 120 gets close to a high-refresh panel.", FPS.iter().map(|(l, _)| *l).collect(), FPS.iter().position(|(_, v)| *v == current.fps), |s, i| s.fps = FPS[i].1));
        list.append(&popout::divider());
        list.append(&self.choice("Quality", "Higher is crisper and larger.", QUALITY.iter().map(|(l, _)| *l).collect(), QUALITY.iter().position(|(_, v)| *v == current.quality), |s, i| s.quality = QUALITY[i].1.into()));
        list.append(&popout::divider());
        list.append(&self.choice("Codec", "H.264 plays everywhere; HEVC is crisper; AV1 is best on newer GPUs.", CODEC.iter().map(|(l, _)| *l).collect(), CODEC.iter().position(|(_, v)| *v == current.codec), |s, i| s.codec = CODEC[i].1.into()));
        self.content.append(&card(&list));

        self.content.append(&popout::eyebrow(s, "Encoder"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.append(&self.choice("Encoder", "GPU encoding is fast and barely touches the CPU; CPU is the fallback if the GPU encoder misbehaves.", ENCODER.iter().map(|(l, _)| *l).collect(), ENCODER.iter().position(|(_, v)| *v == current.encoder), |s, i| s.encoder = ENCODER[i].1.into()));
        self.content.append(&card(&list));

        let all = recordings();
        self.content.append(&popout::eyebrow(s, &format!("Recordings · {}", all.len())));
        if all.is_empty() {
            self.content.append(&ui::note(s, "videocam", "No recordings yet."));
            return;
        }
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for (i, rec) in all.iter().enumerate() {
            if i > 0 {
                list.append(&popout::divider());
            }
            let row = line(s);
            let name = TextStyle::new(11.0 * s, pango::Weight::Medium, CREAM).label(&rec.label);
            name.set_hexpand(true);
            name.set_xalign(0.0);
            name.set_ellipsize(pango::EllipsizeMode::End);
            row.append(&name);
            let size = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label(&rec.size);
            size.set_valign(gtk::Align::Center);
            row.append(&size);
            let path = rec.path.clone();
            row.append(&super::icon_button(s, "play_arrow", CREAM, "Play", {
                let path = path.clone();
                move || super::open(&path)
            }));
            row.append(&super::icon_button(s, "folder", theme::ICON_DIM, "Open the folder", || super::open(&recordings_dir())));
            let me = Rc::downgrade(self);
            row.append(&super::icon_button(s, "delete", RED, "Move to the trash", move || {
                super::trash(&path);
                if let Some(me) = me.upgrade() {
                    me.show();
                }
            }));
            list.append(&row);
        }
        self.content.append(&card(&list));
    }

    /// A row with a label, a note and a segmented control saving at once.
    fn choice(self: &Rc<Self>, label: &str, note: &str, labels: Vec<&str>, chosen: Option<usize>, apply: fn(&mut Settings, usize)) -> gtk::Box {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        let row = line(s);
        let words = gtk::Box::new(gtk::Orientation::Vertical, px(3.0));
        words.set_hexpand(true);
        words.set_valign(gtk::Align::Center);
        let name = TextStyle::new(11.5 * s, pango::Weight::Semibold, CREAM).label(label);
        name.set_xalign(0.0);
        words.append(&name);
        let note = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(note);
        note.set_xalign(0.0);
        note.set_wrap(true);
        words.append(&note);
        row.append(&words);
        let me = Rc::downgrade(self);
        let control = ui::segmented(s, &labels, chosen.unwrap_or(usize::MAX), move |i| {
            let mut s = settings();
            apply(&mut s, i);
            save_settings(&s);
            if let Some(me) = me.upgrade() {
                // The encoder readout follows the codec and encoder.
                me.show();
            }
        });
        control.set_valign(gtk::Align::Center);
        row.append(&control);
        row
    }
}

fn line(s: f64) -> gtk::Box {
    let px = |v: f64| (v * s).round() as i32;
    let line = gtk::Box::new(gtk::Orientation::Horizontal, px(12.0));
    line.set_margin_start(px(16.0));
    line.set_margin_end(px(16.0));
    line.set_margin_top(px(10.0));
    line.set_margin_bottom(px(10.0));
    line
}

/// A card around `content`, boxed so the card counts its margins.
fn card(content: &impl IsA<gtk::Widget>) -> gtk::Overlay {
    let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wrap.append(content);
    popout::card(&wrap)
}
