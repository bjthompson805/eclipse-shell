//! Look-and-feel settings. These mirror the values from the current desktop
//! setup; a settings file can replace them later.

/// Global text/element scale on top of the per-monitor scale.
pub const FONT_SCALE: f64 = 1.3;
/// Bar band height, before scaling.
pub const BAR_HEIGHT: f64 = 30.0;
/// How far the screen frame shows past the screen edge, in logical px.
pub const FRAME_EDGE: f64 = 9.0;
/// Inner corner radius of the screen frame, in logical px.
pub const FRAME_RADIUS: f64 = 9.0;
pub const FRAME_BORDER_WIDTH: f64 = 1.5;
pub const SHADOW_SIZE: f64 = 12.0;
pub const SHADOW_STRENGTH: f64 = 0.63;

/// Launcher card corner radius, in logical px (not scaled, like window rounding).
pub const LAUNCHER_RADIUS: f64 = 16.0;
/// Compositor blur size forced while the launcher is open; 0 leaves blur alone.
pub const LAUNCHER_BLUR: i64 = 12;
pub const LAUNCHER_GREETING: bool = true;
pub const LAUNCHER_WEATHER: bool = true;
/// "Town, Region" for the weather; empty locates by IP address instead.
pub const WEATHER_LOCATION: &str = "Sisters, Oregon";

pub const CLOCK_24H: bool = false;
pub const CLOCK_SECONDS: bool = true;

/// Show the CPU, memory and temperature readout.
pub const BAR_STATS: bool = true;
/// Show Claude Code's five-hour rate-limit usage.
pub const BAR_AGENT_USAGE: bool = true;
/// Show download and upload speed.
pub const BAR_NET_SPEED: bool = true;
/// Show volume, brightness, network, Bluetooth and battery glyphs.
pub const BAR_STATUS: bool = true;
/// Show the current weather (from WEATHER_LOCATION).
pub const BAR_WEATHER: bool = true;
/// Show system tray icons.
pub const BAR_TRAY: bool = true;
/// Show the playing track.
pub const BAR_MEDIA: bool = true;

/// Show only workspaces up to the highest occupied one (plus the active one).
pub const OCCUPIED_WORKSPACES_ONLY: bool = true;
/// Show per-window app icons inside occupied workspace pills.
pub const WORKSPACE_ICONS: bool = true;

pub const FONT: &str = "JetBrainsMono Nerd Font";
pub const MONO_FONT: &str = "JetBrainsMono Nerd Font";
pub const ICON_FONT: &str = "Material Symbols Rounded";
/// App icon theme, matching the Qt side of the desktop (qt6ct).
pub const ICON_THEME: &str = "Papirus-Dark";

/// Scale for bar-side elements on a monitor `height` logical px tall.
pub fn bar_scale(height: i32) -> f64 {
    f64::from(height) / 1080.0 * FONT_SCALE.clamp(0.7, 1.6)
}

/// Scale for the launcher, capped so tall monitors don't balloon it.
pub fn launcher_scale(height: i32) -> f64 {
    (f64::from(height) / 1080.0).min(1.2) * FONT_SCALE.clamp(0.8, 1.4)
}
