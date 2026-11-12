//! CPU, memory and temperature readout: a glyph and a percentage each, tinted
//! with the accent once a value runs hot.
//!
//! Values come from one `sysctl` call every two seconds. Memory counts free
//! and inactive pages plus the ZFS ARC as available, since the ARC gives
//! memory back on demand.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, SUBTLE};

const POLL: Duration = Duration::from_secs(2);
const SYSCTLS: &[&str] = &[
    "kern.cp_times",
    "hw.physmem",
    "hw.pagesize",
    "vm.stats.vm.v_free_count",
    "vm.stats.vm.v_inactive_count",
    "kstat.zfs.misc.arcstats.size",
    "dev.cpu.0.temperature",
    "hw.acpi.thermal.tz0.temperature",
];

#[derive(Default, Clone, Copy)]
struct Sample {
    cpu: Option<u32>,
    mem: Option<u32>,
    temp: Option<u32>,
}

struct Item {
    row: gtk::Box,
    glyph: gtk::Label,
    value: gtk::Label,
    name: &'static str,
    warn_above: u32,
}

impl Item {
    fn new(s: f64, name: &'static str, warn_above: u32) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, (3.0 * s).round() as i32);
        let glyph = theme::icon_glyph(name, 14.0 * s, SUBTLE);
        let value = gtk::Label::new(None);
        row.append(&glyph);
        row.append(&value);
        let item = Self { row, glyph, value, name, warn_above };
        item.show(s, None, "");
        item
    }

    fn show(&self, s: f64, value: Option<u32>, unit: &str) {
        self.row.set_visible(value.is_some());
        let Some(value) = value else { return };
        let color = if value > self.warn_above { theme::palette().accent_lit } else { SUBTLE };
        let glyph = theme::filled_icon_glyph(self.name, 14.0 * s, color);
        self.glyph.set_attributes(glyph.attributes().as_ref());
        self.value.set_text(&format!("{value}{unit}"));
        TextStyle::new(9.5 * s, pango::Weight::Medium, color).apply(&self.value);
    }
}

pub fn stats(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (11.0 * s).round() as i32);
    let cpu = Item::new(s, "memory", 85);
    let mem = Item::new(s, "developer_board", 90);
    let temp = Item::new(s, "device_thermostat", 80);
    for item in [&cpu, &mem, &temp] {
        row.append(&item.row);
    }

    let last = Rc::new(Cell::new(Sample::default()));
    let paint = Rc::new({
        let last = last.clone();
        move || {
            let sample = last.get();
            cpu.show(s, sample.cpu, "%");
            mem.show(s, sample.mem, "%");
            temp.show(s, sample.temp, "\u{b0}");
        }
    });
    theme::on_palette_change({
        let paint = paint.clone();
        move || paint()
    });

    let owner = row.downgrade();
    glib::spawn_future_local(async move {
        let mut prev_times: Vec<u64> = Vec::new();
        while owner.upgrade().is_some() {
            if let Ok(text) = read_sysctls().await {
                last.set(parse(&text, &mut prev_times));
                paint();
            }
            glib::timeout_future(POLL).await;
        }
    });
    row
}

async fn read_sysctls() -> Result<String, glib::Error> {
    let mut argv: Vec<&std::ffi::OsStr> = vec!["sysctl".as_ref(), "-i".as_ref()];
    argv.extend(SYSCTLS.iter().map(std::ffi::OsStr::new));
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE)?;
    let (stdout, _) = process.communicate_utf8_future(None).await?;
    Ok(stdout.map(String::from).unwrap_or_default())
}

fn parse(text: &str, prev_times: &mut Vec<u64>) -> Sample {
    let values: HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once(": ")).map(|(k, v)| (k, v.trim())).collect();
    let num = |key: &str| values.get(key).and_then(|v| v.parse::<f64>().ok());

    // kern.cp_times holds user, nice, sys, intr, idle ticks for every core.
    let mut cpu = None;
    if let Some(raw) = values.get("kern.cp_times") {
        let times: Vec<u64> = raw.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if times.len() == prev_times.len() && !times.is_empty() {
            let (mut total, mut idle) = (0u64, 0u64);
            for (i, (now, before)) in times.iter().zip(prev_times.iter()).enumerate() {
                let delta = now.saturating_sub(*before);
                total += delta;
                if i % 5 == 4 {
                    idle += delta;
                }
            }
            if total > 0 {
                cpu = Some((100.0 * (1.0 - idle as f64 / total as f64)).round().clamp(0.0, 100.0) as u32);
            }
        }
        *prev_times = times;
    }

    let mem = match (num("hw.physmem"), num("hw.pagesize")) {
        (Some(physmem), Some(page)) if physmem > 0.0 => {
            let pages = num("vm.stats.vm.v_free_count").unwrap_or(0.0) + num("vm.stats.vm.v_inactive_count").unwrap_or(0.0);
            let available = pages * page + num("kstat.zfs.misc.arcstats.size").unwrap_or(0.0);
            Some((100.0 * (physmem - available) / physmem).round().clamp(0.0, 100.0) as u32)
        }
        _ => None,
    };

    // Temperatures read like "39.1C".
    let temp = ["dev.cpu.0.temperature", "hw.acpi.thermal.tz0.temperature"]
        .iter()
        .find_map(|k| values.get(k).and_then(|v| v.trim_end_matches('C').parse::<f64>().ok()))
        .map(|t| t.round().max(0.0) as u32);

    Sample { cpu, mem, temp }
}
