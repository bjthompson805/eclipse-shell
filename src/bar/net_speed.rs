//! Network throughput: download over upload, for the interface that carries
//! the default route.
//!
//! Byte counters come from `netstat -ibn` every 1.5 seconds. The value column
//! is as wide as the widest reading ("1023.9 KB/s"), so moving between B/s,
//! KB/s and MB/s never shifts the modules beside it.

use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, SUBTLE};

const POLL: Duration = Duration::from_millis(1500);
const WIDEST: &str = "1023.9 KB/s";
const READ: &str = "i=$(route -n get default 2>/dev/null | awk '/interface:/{print $2}'); \
                    [ -n \"$i\" ] && netstat -ibn -I \"$i\"";

pub fn net_speed(s: f64) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, (1.0 * s).round() as i32);
    let style = TextStyle::new(8.5 * s, pango::Weight::Medium, SUBTLE);
    let row = |glyph: &str| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, (3.0 * s).round() as i32);
        row.append(&theme::filled_icon_glyph(glyph, 10.0 * s, SUBTLE));
        let value = style.label("\u{2013}");
        value.set_xalign(0.0);
        value.set_width_chars(WIDEST.chars().count() as i32);
        row.append(&value);
        column.append(&row);
        value
    };
    let down = row("arrow_downward");
    let up = row("arrow_upward");

    let owner = column.downgrade();
    glib::spawn_future_local(async move {
        let mut prev: Option<(String, u64, u64, Instant)> = None;
        while owner.upgrade().is_some() {
            match read_counters().await {
                Some((iface, rx, tx)) => {
                    let now = Instant::now();
                    if let Some((p_iface, p_rx, p_tx, p_at)) = &prev
                        && *p_iface == iface
                    {
                        let secs = now.duration_since(*p_at).as_secs_f64().max(0.001);
                        down.set_text(&format_rate(rx.saturating_sub(*p_rx) as f64 / secs));
                        up.set_text(&format_rate(tx.saturating_sub(*p_tx) as f64 / secs));
                    }
                    prev = Some((iface, rx, tx, now));
                }
                None => {
                    prev = None;
                    down.set_text("\u{2013}");
                    up.set_text("\u{2013}");
                }
            }
            glib::timeout_future(POLL).await;
        }
    });
    column
}

/// The default-route interface and its received and sent byte counts.
async fn read_counters() -> Option<(String, u64, u64)> {
    let process = gio::Subprocess::newv(
        &["sh", "-c", READ].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    // The link-level row carries the interface's own totals:
    // Name Mtu <Link#n> [Address] Ipkts Ierrs Idrop Ibytes Opkts Oerrs Obytes Coll
    // Tunnels have no address, so count from the end.
    let line = stdout?.lines().find(|l| l.split_whitespace().nth(2).is_some_and(|n| n.starts_with("<Link#")))?.to_owned();
    let fields: Vec<&str> = line.split_whitespace().collect();
    let n = fields.len();
    if n < 11 {
        return None;
    }
    Some((fields[0].to_string(), fields[n - 5].parse().ok()?, fields[n - 2].parse().ok()?))
}

fn format_rate(bytes_per_sec: f64) -> String {
    const K: f64 = 1024.0;
    match bytes_per_sec {
        b if b < K => format!("{} B/s", b.round()),
        b if b < K * K => format!("{:.1} KB/s", b / K),
        b if b < K * K * K => format!("{:.1} MB/s", b / (K * K)),
        b => format!("{:.1} GB/s", b / (K * K * K)),
    }
}
