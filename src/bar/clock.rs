//! Stacked clock: the time over a smaller date.

use gtk::prelude::*;
use gtk::{glib, pango};
use gtk4 as gtk;

use crate::config;
use crate::theme::{TextStyle, CREAM, DIM};

pub fn time_text(now: &glib::DateTime) -> String {
    let format = match (config::get().clock_24h, config::get().clock_seconds) {
        (true, true) => "%H:%M:%S",
        (true, false) => "%H:%M",
        (false, true) => "%-I:%M:%S %p",
        (false, false) => "%-I:%M %p",
    };
    now.format(format).map(String::from).unwrap_or_default()
}

pub fn clock(s: f64) -> gtk::Box {
    let time = TextStyle::new(12.0 * s, pango::Weight::Semibold, CREAM).label("");
    let date = TextStyle::new(12.0 * 0.62 * s, pango::Weight::Medium, DIM).label("");
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&time);
    column.append(&date);

    let update = move |now: &glib::DateTime| {
        time.set_text(&time_text(now));
        date.set_text(&now.format("%a %-d %b").map(String::from).unwrap_or_default());
    };
    every_second(&column, update);
    column
}

/// Calls `tick` now and then just after each wall-clock second turns over,
/// for as long as `owner` is alive.
pub fn every_second(owner: &impl IsA<gtk::Widget>, tick: impl Fn(&glib::DateTime) + 'static) {
    fn schedule(owner: glib::WeakRef<gtk::Widget>, tick: std::rc::Rc<dyn Fn(&glib::DateTime)>) {
        let Ok(now) = glib::DateTime::now_local() else { return };
        tick(&now);
        let wait = 1000 - u64::from(now.microsecond() as u32 / 1000) + 5;
        glib::timeout_add_local_once(std::time::Duration::from_millis(wait), move || {
            if owner.upgrade().is_some() {
                schedule(owner, tick);
            }
        });
    }
    schedule(owner.upcast_ref::<gtk::Widget>().downgrade(), std::rc::Rc::new(tick));
}
