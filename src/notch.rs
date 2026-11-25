//! The camera cutout on the M5 MacBook Air 15"'s built-in panel.
//!
//! With the display driver's full-height mode on, the panel is 2880x1864 and
//! its top 64 rows run either side of the cutout. The bar claims that strip,
//! so everything else lays out as on the 2880x1800 area below it.

use gtk::gdk;
use gtk::prelude::*;
use gtk4 as gtk;

/// The panel in native pixels. The cutout spans columns 1285-1596 (measured
/// in macOS), so each side of the strip is ~1284 px wide.
const PANEL: (f64, f64) = (2880.0, 1864.0);
const STRIP: f64 = 64.0;

/// The strip on one monitor, in its logical px.
#[derive(Clone, Copy)]
pub struct Notch {
    pub height: f64,
}

/// The cutout, if `monitor` is the built-in panel in its full-height mode.
pub fn notch(monitor: &gdk::Monitor) -> Option<Notch> {
    if !monitor.connector().is_some_and(|c| c.starts_with("eDP")) {
        return None;
    }
    let g = monitor.geometry();
    let k = PANEL.0 / f64::from(g.width());
    // The logical height is rounded, so allow a pixel's slack at any scale.
    if (f64::from(g.height()) * k - PANEL.1).abs() > k {
        return None;
    }
    Some(Notch { height: STRIP / k })
}

/// How far the strip pushes the bar down on `monitor`: 0 without a cutout.
pub fn strip(monitor: &gdk::Monitor) -> f64 {
    notch(monitor).map_or(0.0, |n| n.height)
}

/// The monitor's height below the strip, which sizes the shell.
pub fn height(monitor: &gdk::Monitor) -> i32 {
    monitor.geometry().height() - strip(monitor).round() as i32
}
