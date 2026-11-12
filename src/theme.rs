//! Colours, fonts and shape helpers. Nothing here lives in a stylesheet.
//!
//! Surface and accent colours follow the wallpaper: wallust writes its palette
//! to ~/.cache/wallust/colors.json, and the shell derives a dark surface ramp
//! and a readable accent from it, refreshing whenever the file changes.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;

use gtk::prelude::*;
use gtk::{cairo, gio, glib, pango};
use gtk4 as gtk;

use crate::config;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self { r: r as f64 / 255.0, g: g as f64 / 255.0, b: b as f64 / 255.0, a: 1.0 }
    }

    pub const fn rgba8(r: u8, g: u8, b: u8, a: f64) -> Self {
        Self { r: r as f64 / 255.0, g: g as f64 / 255.0, b: b as f64 / 255.0, a }
    }

    pub fn hex(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#')?;
        let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        Some(Self::rgb8(channel(0)?, channel(2)?, channel(4)?))
    }

    pub fn alpha(self, a: f64) -> Self {
        Self { a, ..self }
    }

    fn to_hsv(self) -> (f64, f64, f64) {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == self.r {
            60.0 * ((self.g - self.b) / delta).rem_euclid(6.0)
        } else if max == self.g {
            60.0 * ((self.b - self.r) / delta + 2.0)
        } else {
            60.0 * ((self.r - self.g) / delta + 4.0)
        };
        let saturation = if max == 0.0 { 0.0 } else { delta / max };
        (hue, saturation, max)
    }

    fn from_hsv(hue: f64, saturation: f64, value: f64) -> Self {
        let c = value * saturation;
        let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = value - c;
        let (r, g, b) = match (hue.rem_euclid(360.0) / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Self { r: r + m, g: g + m, b: b + m, a: 1.0 }
    }

    fn luminance(self) -> f64 {
        let linear = |u: f64| if u <= 0.04045 { u / 12.92 } else { ((u + 0.055) / 1.055).powf(2.4) };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    fn contrast(self, other: Self) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    pub fn to_u16(self) -> (u16, u16, u16) {
        let c = |v: f64| (v.clamp(0.0, 1.0) * f64::from(u16::MAX)).round() as u16;
        (c(self.r), c(self.g), c(self.b))
    }

    pub fn to_hex(self) -> String {
        let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", c(self.r), c(self.g), c(self.b))
    }
}

// Fixed foreground colours, warm off-whites stepping down in emphasis.
pub const BRIGHT: Rgba = Rgba::rgb8(0xf3, 0xed, 0xe1);
pub const CREAM: Rgba = Rgba::rgb8(0xe6, 0xdc, 0xcb);
pub const SUBTLE: Rgba = Rgba::rgb8(0xc7, 0xbf, 0xae);
pub const DIM: Rgba = Rgba::rgb8(0x8f, 0x87, 0x70);
pub const ICON_DIM: Rgba = DIM;
pub const FAINT: Rgba = Rgba::rgb8(0x5c, 0x52, 0x49);
pub const HAIR: Rgba = Rgba::rgba8(243, 237, 225, 0.12);
pub const SHEEN: Rgba = Rgba::rgba8(243, 237, 225, 0.06);
/// Tinted fill and edge for selected rows and active chips.
pub const CHIP_FILL: Rgba = Rgba::rgba8(226, 52, 42, 0.10);
pub const CHIP_EDGE: Rgba = Rgba::rgba8(243, 237, 225, 0.18);
pub const ROW_HOVER: Rgba = Rgba { r: 0.94, g: 0.88, b: 0.84, a: 0.03 };
pub const SCRIM: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.35 };
// Sky colours for the launcher's day/night arc; deliberately not wallpaper-tinted.
pub const SUN_GOLD: Rgba = Rgba::rgb8(0xff, 0xc7, 0x77);
pub const MOON_GLOW: Rgba = Rgba::rgb8(0x7a, 0xa2, 0xf7);
pub const MOON_DISC: Rgba = Rgba::rgb8(0xc8, 0xd3, 0xf5);
pub const NIGHT_GHOST: Rgba = Rgba::rgb8(0x41, 0x48, 0x68);

/// Wallpaper-derived colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Frame and card surface.
    pub base: Rgba,
    /// Raised surfaces: bar module pills.
    pub raised: Rgba,
    /// Recessed surfaces: cards inside the launcher.
    pub recessed: Rgba,
    /// Card outlines.
    pub outline: Rgba,
    pub accent: Rgba,
    pub accent_lit: Rgba,
    /// The frame's inner edge, the same hue as window borders.
    pub edge: Rgba,
}

impl Palette {
    fn from_wallust(background: Rgba, color4: Rgba) -> Self {
        let base = clamp_to_dark_band(background);
        let raised = shift_value(base, 0.05);
        let accent = readable_on(vivid(color4), raised, 3.0);
        Self {
            base,
            raised,
            recessed: shift_value(base, -0.03),
            outline: shift_value(base, 0.14),
            accent,
            accent_lit: lighter(accent, 1.22),
            edge: color4,
        }
    }

    fn fallback() -> Self {
        Self::from_wallust(Rgba::rgb8(0x16, 0x11, 0x0b), Rgba::rgb8(0xe2, 0x34, 0x2a))
    }
}

/// Keeps the wallpaper's hue but lands its brightness in a dark range the
/// light text stays readable on: very dark lifts slightly, bright compresses
/// hard, and saturation is capped so it reads as a tint rather than neon.
fn clamp_to_dark_band(c: Rgba) -> Rgba {
    let (hue, saturation, value) = c.to_hsv();
    let value = if value < 0.08 {
        0.08
    } else if value > 0.26 {
        0.26 + (value - 0.26) * 0.06
    } else {
        value
    };
    Rgba::from_hsv(hue, saturation.min(0.55), value)
}

fn shift_value(c: Rgba, delta: f64) -> Rgba {
    let (hue, saturation, value) = c.to_hsv();
    Rgba::from_hsv(hue, saturation, (value + delta).clamp(0.0, 1.0))
}

/// Pushes a muted colour towards a clear, bright version of itself.
fn vivid(c: Rgba) -> Rgba {
    let (hue, saturation, value) = c.to_hsv();
    let saturation = if saturation < 0.06 { 0.0 } else { (saturation * 1.2 + 0.06).min(1.0) };
    Rgba::from_hsv(hue, saturation, value.max(0.74))
}

/// Blends `fg` towards white until it reaches `ratio` contrast against `bg`.
fn readable_on(mut fg: Rgba, bg: Rgba, ratio: f64) -> Rgba {
    for _ in 0..8 {
        if fg.contrast(bg) >= ratio {
            break;
        }
        fg = Rgba { r: fg.r + (1.0 - fg.r) * 0.18, g: fg.g + (1.0 - fg.g) * 0.18, b: fg.b + (1.0 - fg.b) * 0.18, a: 1.0 };
    }
    fg
}

/// Brightens by `factor`; past full brightness the excess comes off the
/// saturation instead, so the colour heads towards white.
fn lighter(c: Rgba, factor: f64) -> Rgba {
    let (hue, saturation, value) = c.to_hsv();
    let value = value * factor;
    if value > 1.0 {
        Rgba::from_hsv(hue, (saturation - (value - 1.0)).max(0.0), 1.0)
    } else {
        Rgba::from_hsv(hue, saturation, value)
    }
}

thread_local! {
    static PALETTE: Cell<Palette> = Cell::new(Palette::fallback());
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
    static MONITOR: RefCell<Option<gio::FileMonitor>> = RefCell::default();
}

pub fn palette() -> Palette {
    PALETTE.with(Cell::get)
}

/// Runs `f` whenever the palette changes, e.g. to queue a redraw.
pub fn on_palette_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

fn wallust_path() -> std::path::PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME").map(Into::into).unwrap_or_else(|| glib::home_dir().join(".cache"));
    cache.join("wallust").join("colors.json")
}

fn load_palette() -> Option<Palette> {
    let text = std::fs::read_to_string(wallust_path()).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let colour = |key: &str| json.get(key)?.as_str().and_then(Rgba::hex);
    Some(Palette::from_wallust(colour("background")?, colour("color4")?))
}

/// Loads the palette and keeps it in sync with the wallpaper.
pub fn watch_palette() {
    if let Some(palette) = load_palette() {
        PALETTE.with(|p| p.set(palette));
    }
    let file = gio::File::for_path(wallust_path());
    match file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
        Ok(monitor) => {
            monitor.connect_changed(|_, _, _, event| {
                if !matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created) {
                    return;
                }
                let Some(palette) = load_palette() else { return };
                if PALETTE.with(|p| p.replace(palette)) != palette {
                    LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
                }
            });
            MONITOR.with(|m| *m.borrow_mut() = Some(monitor));
        }
        Err(err) => eprintln!("eclipse-shell: not watching the wallpaper palette: {err}"),
    }
}

// ---- text -------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct TextStyle {
    pub family: &'static str,
    /// Logical pixels.
    pub px: f64,
    pub weight: pango::Weight,
    pub color: Rgba,
    /// Logical pixels between letters.
    pub tracking: f64,
}

impl TextStyle {
    pub fn new(px: f64, weight: pango::Weight, color: Rgba) -> Self {
        Self { family: config::FONT, px, weight, color, tracking: 0.0 }
    }

    pub fn mono(self) -> Self {
        Self { family: config::MONO_FONT, ..self }
    }

    pub fn tracking(self, px: f64) -> Self {
        Self { tracking: px, ..self }
    }

    pub fn font(&self) -> pango::FontDescription {
        let mut font = pango::FontDescription::new();
        font.set_family(self.family);
        font.set_weight(self.weight);
        font.set_absolute_size(self.px * f64::from(pango::SCALE));
        font
    }

    pub fn attrs(&self) -> pango::AttrList {
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrFontDesc::new(&self.font()));
        let (r, g, b) = self.color.to_u16();
        attrs.insert(pango::AttrColor::new_foreground(r, g, b));
        if self.color.a < 1.0 {
            attrs.insert(pango::AttrInt::new_foreground_alpha((self.color.a * f64::from(u16::MAX)) as u16));
        }
        if self.tracking != 0.0 {
            attrs.insert(pango::AttrInt::new_letter_spacing((self.tracking * f64::from(pango::SCALE)) as i32));
        }
        // Tabular figures keep clocks and counters from jittering.
        attrs.insert(pango::AttrFontFeatures::new("tnum 1"));
        attrs
    }

    pub fn label(&self, text: &str) -> gtk::Label {
        let label = gtk::Label::new(Some(text));
        label.set_attributes(Some(&self.attrs()));
        label
    }

    pub fn apply(&self, label: &gtk::Label) {
        label.set_attributes(Some(&self.attrs()));
    }
}

/// A Material Symbols glyph, picked by ligature name.
pub fn icon_glyph(name: &str, px: f64, color: Rgba) -> gtk::Label {
    glyph(name, px, color, "FILL=0,opsz=20")
}

/// The solid variant of [`icon_glyph`].
pub fn filled_icon_glyph(name: &str, px: f64, color: Rgba) -> gtk::Label {
    glyph(name, px, color, "FILL=1,opsz=20")
}

fn glyph(name: &str, px: f64, color: Rgba, variations: &str) -> gtk::Label {
    let style = TextStyle { family: config::ICON_FONT, px, weight: pango::Weight::Medium, color, tracking: 0.0 };
    let label = style.label(name);
    let attrs = style.attrs();
    let mut font = style.font();
    font.set_variations(Some(variations));
    attrs.insert(pango::AttrFontDesc::new(&font));
    label.set_attributes(Some(&attrs));
    label
}

// ---- shapes -----------------------------------------------------------------

pub fn set_source(cr: &cairo::Context, c: Rgba) {
    cr.set_source_rgba(c.r, c.g, c.b, c.a);
}

pub fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// A rectangle whose corners are quarter superellipses (|x|^4 + |y|^4 = 1),
/// the same corner shape Hyprland gives windows.
pub fn squircle(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let corners = [(x + r, y + r, PI), (x + w - r, y + r, 1.5 * PI), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, 0.5 * PI)];
    cr.new_sub_path();
    for (cx, cy, start) in corners {
        for step in 0..=12 {
            let angle = start + 0.5 * PI * f64::from(step) / 12.0;
            let (sin, cos) = angle.sin_cos();
            // Exponent 2/4: the superellipse parametrisation for power 4.
            let px = cx + r * cos.signum() * cos.abs().sqrt();
            let py = cy + r * sin.signum() * sin.abs().sqrt();
            cr.line_to(px, py);
        }
    }
    cr.close_path();
}
