//! Display popout, dropping from the sun in the status cluster: the panel
//! backlight and screen vibrance.
//!
//! The backlight goes through backlight(8). Vibrance is a saturation shader
//! set as Hyprland's screen_shader; 50% is the screen untouched, so no
//! shader at all. Its value is saved and applied again at startup.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::fader::{self, Fader};
use crate::hyprland;
use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, TextStyle, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-display";
const TICK: Duration = Duration::from_secs(1);
const WIDTH: f64 = 300.0;
/// The dimmest the panel goes, so it never turns off outright.
const MIN_BRIGHTNESS: f64 = 0.05;
const NEUTRAL_VIBRANCE: u32 = 50;

pub struct DisplayPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
    brightness: RefCell<Option<Rc<Fader>>>,
    vibrance: RefCell<Option<Rc<Fader>>>,
}

impl DisplayPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self { popout: Popout::new(app, NAMESPACE), generation: Cell::new(0), brightness: RefCell::default(), vibrance: RefCell::default() })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let display = self.clone();
        glib::spawn_future_local(async move {
            let brightness = read_brightness().await;
            let s = display.popout.scale();
            display.popout.set_content(&popout::card(&display.body(s, brightness)));
            display.popout.show(Anchor::Under(x));
        });

        // Brightness keys and idle dimming change the backlight behind our back.
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let display = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match display.upgrade() {
            Some(d) if d.popout.is_open() && d.generation.get() == generation => {
                glib::spawn_future_local(async move {
                    let fader = d.brightness.borrow().clone();
                    if let (Some(fader), Some(value)) = (fader, read_brightness().await) {
                        fader.show(value, false);
                    }
                });
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    fn body(&self, s: f64, brightness: Option<f64>) -> gtk::Box {
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

        let header = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        header.append(&theme::filled_icon_glyph("light_mode", 16.0 * s, theme::palette().accent));
        header.append(&TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("DISPLAY"));
        column.append(&header);

        let faders = gtk::Box::new(gtk::Orientation::Vertical, px(6.0));
        let options = |glyph, floor, send_every| fader::Options { glyphs: (glyph, glyph), glyph_px: 17.0 * s, inline: true, floor, send_every: Duration::from_millis(send_every) };
        *self.brightness.borrow_mut() = brightness.map(|value| {
            let fader = Fader::new(s, value, false, options("light_mode", MIN_BRIGHTNESS, 40), set_brightness, None);
            faders.append(&fader.widget);
            fader
        });
        // Each change recompiles the shader, so a drag sends fewer of them.
        let vibrance = Fader::new(s, f64::from(saved_vibrance()) / 100.0, false, options("contrast", 0.0, 120), |v| set_vibrance((v * 100.0).round() as u32), None);
        faders.append(&vibrance.widget);
        *self.vibrance.borrow_mut() = Some(vibrance);
        column.append(&faders);
        column
    }
}

/// Applies the saved vibrance, if one was saved.
pub fn restore() {
    if let Some(pct) = std::fs::read_to_string(vibrance_file()).ok().and_then(|v| v.trim().parse().ok()) {
        apply_vibrance(pct);
    }
}

async fn read_brightness() -> Option<f64> {
    let process = gio::Subprocess::newv(&["backlight".as_ref(), "-q".as_ref()], gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    let percent: f64 = stdout?.trim().parse().ok()?;
    Some(percent / 100.0)
}

fn set_brightness(value: f64) {
    let percent = (value * 100.0).round().to_string();
    if let Ok(p) = gio::Subprocess::newv(&["backlight".as_ref(), percent.as_ref()], gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        p.wait_async(gio::Cancellable::NONE, |_| {});
    }
}

fn state_dir() -> PathBuf {
    glib::user_state_dir().join("eclipse-shell")
}

fn vibrance_file() -> PathBuf {
    state_dir().join("vibrance")
}

fn saved_vibrance() -> u32 {
    std::fs::read_to_string(vibrance_file()).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(NEUTRAL_VIBRANCE)
}

fn set_vibrance(pct: u32) {
    apply_vibrance(pct);
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(vibrance_file(), format!("{pct}\n"));
}

thread_local! {
    /// Which of two shader files is live: Hyprland only reloads the shader
    /// when its path changes, so writes take turns between them.
    static SLOT: Cell<bool> = const { Cell::new(false) };
}

/// 0-100% scales saturation by 0-2; 50% is the screen as it is.
fn apply_vibrance(pct: u32) {
    let pct = pct.min(100);
    if pct == NEUTRAL_VIBRANCE {
        hyprland::eval(r#"hl.config({ decoration = { screen_shader = "" } })"#);
        return;
    }
    let slot = SLOT.with(|s| {
        s.set(!s.get());
        s.get()
    });
    let path = state_dir().join(if slot { "vibrance-a.glsl" } else { "vibrance-b.glsl" });
    let saturation = f64::from(pct) / 50.0;
    let shader = format!(
        "#version 300 es\n\
         precision highp float;\n\
         in vec2 v_texcoord;\n\
         uniform sampler2D tex;\n\
         out vec4 fragColor;\n\
         void main() {{\n\
         \x20   vec4 color = texture(tex, v_texcoord);\n\
         \x20   float luma = dot(color.rgb, vec3(0.2126, 0.7152, 0.0722));\n\
         \x20   fragColor = vec4(clamp(mix(vec3(luma), color.rgb, {saturation:.3}), 0.0, 1.0), color.a);\n\
         }}\n"
    );
    let _ = std::fs::create_dir_all(state_dir());
    if std::fs::write(&path, shader).is_ok() {
        hyprland::eval(&format!(r#"hl.config({{ decoration = {{ screen_shader = "{}" }} }})"#, path.display()));
    }
}
