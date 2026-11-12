//! Look-and-feel settings, read once at start from
//! `~/.config/eclipse-shell/settings.json`. Anything the file leaves out
//! keeps its default; the hub (`eclipse-shell hub`) writes the file.

use std::path::PathBuf;
use std::sync::OnceLock;

use gtk4::glib;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Global text/element scale on top of the per-monitor scale.
    pub font_scale: f64,
    /// Bar band height, before scaling.
    pub bar_height: f64,
    /// How far the screen frame shows past the screen edge, in logical px.
    pub frame_edge: f64,
    /// Inner corner radius of the screen frame, in logical px.
    pub frame_radius: f64,
    pub frame_border_width: f64,
    pub shadow_size: f64,
    pub shadow_strength: f64,

    /// Launcher card corner radius, in logical px (not scaled, like window rounding).
    pub launcher_radius: f64,
    /// Compositor blur size forced while the launcher is open; 0 leaves blur alone.
    pub launcher_blur: i64,
    pub launcher_greeting: bool,
    pub launcher_weather: bool,
    /// "Town, Region" for the weather; empty locates by IP address instead.
    pub weather_location: String,

    pub clock_24h: bool,
    pub clock_seconds: bool,

    /// Show the CPU, memory and temperature readout.
    pub bar_stats: bool,
    /// Show Claude Code's five-hour rate-limit usage.
    pub bar_agent_usage: bool,
    /// Show download and upload speed.
    pub bar_net_speed: bool,
    /// Show volume, brightness, network, Bluetooth and battery glyphs.
    pub bar_status: bool,
    /// Show the current weather (from `weather_location`).
    pub bar_weather: bool,
    /// Show system tray icons.
    pub bar_tray: bool,
    /// Show the playing track.
    pub bar_media: bool,

    /// Show only workspaces up to the highest occupied one (plus the active one).
    pub occupied_workspaces_only: bool,
    /// Show per-window app icons inside occupied workspace pills.
    pub workspace_icons: bool,

    pub font: String,
    pub mono_font: String,
    pub icon_font: String,
    /// App icon theme, matching the Qt side of the desktop (qt6ct).
    pub icon_theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_scale: 1.3,
            bar_height: 30.0,
            frame_edge: 9.0,
            frame_radius: 9.0,
            frame_border_width: 1.5,
            shadow_size: 12.0,
            shadow_strength: 0.63,
            launcher_radius: 16.0,
            launcher_blur: 12,
            launcher_greeting: true,
            launcher_weather: true,
            weather_location: "Sisters, Oregon".into(),
            clock_24h: false,
            clock_seconds: true,
            bar_stats: true,
            bar_agent_usage: true,
            bar_net_speed: true,
            bar_status: true,
            bar_weather: true,
            bar_tray: true,
            bar_media: true,
            occupied_workspaces_only: true,
            workspace_icons: true,
            font: "JetBrainsMono Nerd Font".into(),
            mono_font: "JetBrainsMono Nerd Font".into(),
            icon_font: "Material Symbols Rounded".into(),
            icon_theme: "Papirus-Dark".into(),
        }
    }
}

pub fn path() -> PathBuf {
    glib::user_config_dir().join("eclipse-shell").join("settings.json")
}

/// The settings file as it stands, or the defaults where it's missing or unreadable.
pub fn load() -> Settings {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| {
            serde_json::from_str(&text).map_err(|err| eprintln!("eclipse-shell: {}: {err}", path().display())).ok()
        })
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    // Written aside and renamed, so a reader never sees half a file.
    let partial = path.with_extension("json.part");
    std::fs::write(&partial, text + "\n")?;
    std::fs::rename(partial, path)
}

/// The settings this process started with.
pub fn get() -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(load)
}

/// Scale for bar-side elements on a monitor `height` logical px tall.
pub fn bar_scale(height: i32) -> f64 {
    f64::from(height) / 1080.0 * get().font_scale.clamp(0.7, 1.6)
}

/// Scale for the launcher, capped so tall monitors don't balloon it.
pub fn launcher_scale(height: i32) -> f64 {
    (f64::from(height) / 1080.0).min(1.2) * get().font_scale.clamp(0.8, 1.4)
}
