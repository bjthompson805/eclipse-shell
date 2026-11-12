//! Status cluster: volume, display brightness, network, Bluetooth and
//! battery, as a row of glyphs. Scrolling the volume or brightness glyph
//! nudges it.
//!
//! One small shell script gathers every reading every two seconds (wpctl,
//! backlight, ifconfig, hccontrol, sysctl), and again right after a scroll.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use gtk4 as gtk;

use crate::theme::{self, Rgba, TextStyle, FAINT, FLAME, SUBTLE};

const POLL: Duration = Duration::from_secs(2);
const READ: &str = r#"
printf 'vol=%s\n' "$(wpctl get-volume @DEFAULT_AUDIO_SINK@ 2>/dev/null)"
printf 'bright=%s\n' "$(backlight -q 2>/dev/null)"
printf 'route=%s\n' "$(route -n get default 2>/dev/null | awk '/interface:/{print $2}')"
w=$(ifconfig -l | tr ' ' '\n' | grep '^wlan' | head -1)
if [ -n "$w" ]; then
    printf 'wlan=%s\n' "$w"
    ifconfig "$w" | head -1 | grep -q '<UP' && echo 'radio=1'
    printf 'rssi=%s\n' "$(ifconfig "$w" list sta 2>/dev/null | awk 'NR==2{print $5}')"
fi
n=$(hccontrol read_node_list 2>/dev/null | awk 'NR==2{print $1}')
if [ -n "$n" ]; then
    echo 'bt=1'
    printf 'btconn=%s\n' "$(hccontrol -n "$n" read_connection_list 2>/dev/null | grep -c ' OPEN')"
fi
sysctl -i hw.acpi.battery.units hw.acpi.battery.life hw.acpi.battery.state
"#;

#[derive(Default, Clone)]
struct Reading {
    volume: Option<f64>,
    muted: bool,
    brightness: Option<f64>,
    route: String,
    wifi: Option<Wifi>,
    bluetooth: Option<u32>,
    battery: Option<Battery>,
}

#[derive(Clone)]
struct Wifi {
    radio: bool,
    /// Signal over noise, in dB, while associated.
    rssi: Option<f64>,
}

#[derive(Clone, Copy)]
struct Battery {
    percent: u32,
    charging: bool,
}

impl Battery {
    fn low(self) -> bool {
        !self.charging && self.percent <= 20
    }
}

pub fn status(s: f64) -> gtk::Box {
    let glyph_px = 14.0 * s;
    let slot = (glyph_px + 4.0 * s).round() as i32;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (9.0 * s).round() as i32);

    let volume = theme::filled_icon_glyph("volume_up", glyph_px, SUBTLE);
    let sun = gtk::DrawingArea::new();
    let network = theme::filled_icon_glyph("signal_wifi_off", glyph_px, FAINT);
    let bluetooth = theme::filled_icon_glyph("bluetooth", glyph_px, SUBTLE);
    let battery = theme::filled_icon_glyph("battery_full", glyph_px + 1.0 * s, SUBTLE);
    let battery_pct = gtk::Label::new(None);
    for glyph in [&volume, &network, &bluetooth] {
        glyph.set_size_request(slot, slot);
        row.append(glyph);
    }
    row.insert_child_after(&sun, Some(&volume));
    sun.set_size_request(slot, slot);
    let battery_row = gtk::Box::new(gtk::Orientation::Horizontal, (2.0 * s).round() as i32);
    battery_row.append(&battery);
    battery_row.append(&battery_pct);
    row.append(&battery_row);

    let reading = Rc::new(RefCell::new(Reading::default()));
    sun.set_draw_func({
        let reading = reading.clone();
        move |_, cr, w, h| {
            let bright = reading.borrow().brightness.unwrap_or(50.0);
            draw_sun(cr, f64::from(w), f64::from(h), glyph_px, bright > 50.0);
        }
    });

    let paint = Rc::new({
        let (reading, sun, volume) = (reading.clone(), sun.clone(), volume.clone());
        move || {
            let r = reading.borrow();
            let (name, color) = match r.volume {
                _ if r.muted => ("volume_off", FAINT),
                Some(v) if v > 0.5 => ("volume_up", SUBTLE),
                Some(v) if v > 0.0 => ("volume_down", SUBTLE),
                _ => ("volume_mute", SUBTLE),
            };
            set_glyph(&volume, name, glyph_px, color);
            sun.set_visible(r.brightness.is_some());
            sun.queue_draw();

            let (name, color) = network_glyph(&r);
            set_glyph(&network, name, glyph_px, color);

            bluetooth.set_visible(r.bluetooth.is_some());
            let name = if r.bluetooth.unwrap_or(0) > 0 { "bluetooth_connected" } else { "bluetooth" };
            set_glyph(&bluetooth, name, glyph_px, SUBTLE);

            battery_row.set_visible(r.battery.is_some());
            if let Some(b) = r.battery {
                let color = if b.low() {
                    theme::palette().accent_lit
                } else if b.charging {
                    FLAME
                } else {
                    SUBTLE
                };
                set_glyph(&battery, battery_glyph(b), glyph_px + 1.0 * s, color);
                battery_pct.set_text(&format!("{}%", b.percent));
                let text = if b.low() { color } else { SUBTLE };
                TextStyle::new(9.5 * s, pango::Weight::Medium, text).apply(&battery_pct);
            }
        }
    });
    theme::on_palette_change({
        let paint = paint.clone();
        move || paint()
    });

    // Polls on a timer; `refresh` asks for an extra reading right away.
    let (refresh, wakeups) = async_channel::unbounded::<()>();
    let owner = row.downgrade();
    glib::spawn_future_local({
        let (reading, paint) = (reading.clone(), paint.clone());
        async move {
            while owner.upgrade().is_some() {
                if let Some(text) = run(READ).await {
                    *reading.borrow_mut() = parse(&text);
                    paint();
                }
                let tick = glib::timeout_future(POLL);
                let woken = wakeups.recv();
                futures_select(tick, woken).await;
            }
        }
    });

    on_scroll(&volume, {
        let refresh = refresh.clone();
        move |up| {
            let step = if up { "3%+" } else { "3%-" };
            spawn(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
            spawn(&["wpctl", "set-volume", "-l", "1", "@DEFAULT_AUDIO_SINK@", step]);
            let _ = refresh.try_send(());
        }
    });
    on_scroll(&sun, move |up| {
        spawn(&["backlight", if up { "incr" } else { "decr" }, "3"]);
        let _ = refresh.try_send(());
    });
    row
}

fn set_glyph(label: &gtk::Label, name: &str, px: f64, color: Rgba) {
    label.set_text(name);
    label.set_attributes(theme::filled_icon_glyph(name, px, color).attributes().as_ref());
}

fn network_glyph(r: &Reading) -> (&'static str, Rgba) {
    if !r.route.is_empty() && !r.route.starts_with("wlan") {
        return ("lan", SUBTLE);
    }
    let Some(wifi) = &r.wifi else { return ("signal_wifi_off", FAINT) };
    if !wifi.radio {
        return ("wifi_off", FAINT);
    }
    let Some(rssi) = wifi.rssi.filter(|_| r.route.starts_with("wlan")) else { return ("signal_wifi_off", FAINT) };
    // Rough bands for signal over noise: 40 dB and up is as good as it gets.
    let level = (rssi / 40.0).clamp(0.0, 1.0);
    let name = match level {
        l if l > 0.8 => "signal_wifi_4_bar",
        l if l > 0.55 => "network_wifi_3_bar",
        l if l > 0.3 => "network_wifi_2_bar",
        _ => "network_wifi_1_bar",
    };
    (name, SUBTLE)
}

fn battery_glyph(b: Battery) -> &'static str {
    if b.charging {
        return "battery_charging_full";
    }
    match b.percent {
        p if p > 95 => "battery_full",
        p if p > 80 => "battery_6_bar",
        p if p > 65 => "battery_5_bar",
        p if p > 50 => "battery_4_bar",
        p if p > 35 => "battery_3_bar",
        p if p > 20 => "battery_2_bar",
        _ => "battery_1_bar",
    }
}

/// A sun: a disc and eight rays, the rays reaching further when bright.
fn draw_sun(cr: &gtk::cairo::Context, w: f64, h: f64, size: f64, bright: bool) {
    let (cx, cy) = (w / 2.0, h / 2.0);
    let unit = size / 24.0;
    theme::set_source(cr, SUBTLE);
    cr.arc(cx, cy, 4.0 * unit, 0.0, 2.0 * PI);
    let _ = cr.fill();
    cr.set_line_width(2.0 * unit);
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    let (inner, outer) = if bright { (7.5 * unit, 11.0 * unit) } else { (7.5 * unit, 9.0 * unit) };
    for i in 0..8 {
        let a = f64::from(i) * PI / 4.0;
        cr.move_to(cx + inner * a.cos(), cy + inner * a.sin());
        cr.line_to(cx + outer * a.cos(), cy + outer * a.sin());
    }
    let _ = cr.stroke();
}

fn on_scroll(widget: &impl IsA<gtk::Widget>, f: impl Fn(bool) + 'static) {
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.connect_scroll(move |_, _, dy| {
        if dy != 0.0 {
            f(dy < 0.0);
        }
        glib::Propagation::Stop
    });
    widget.add_controller(scroll);
}

fn spawn(argv: &[&str]) {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    if let Ok(p) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        p.wait_async(gio::Cancellable::NONE, |_| {});
    }
}

async fn run(script: &str) -> Option<String> {
    let process = gio::Subprocess::newv(
        &["sh", "-c", script].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    stdout.map(String::from)
}

/// Waits for whichever finishes first.
async fn futures_select(a: impl std::future::Future<Output = ()>, b: impl std::future::Future) {
    let (a, b) = (std::pin::pin!(a), std::pin::pin!(b));
    let mut both = (a, b);
    std::future::poll_fn(|cx| {
        if both.0.as_mut().poll(cx).is_ready() || both.1.as_mut().poll(cx).is_ready() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

fn parse(text: &str) -> Reading {
    let mut v: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        if let Some((k, val)) = line.split_once('=').or_else(|| line.split_once(": ")) {
            v.insert(k.trim(), val.trim());
        }
    }
    let num = |k: &str| v.get(k).and_then(|x| x.parse::<f64>().ok());

    // "Volume: 0.56" or "Volume: 0.56 [MUTED]"
    let vol = v.get("vol").copied().unwrap_or("");
    let volume = vol.split_whitespace().nth(1).and_then(|x| x.parse().ok());
    let wifi = v.get("wlan").map(|_| Wifi { radio: v.contains_key("radio"), rssi: num("rssi") });
    let battery = (num("hw.acpi.battery.units").unwrap_or(0.0) > 0.0)
        .then(|| {
            let percent = num("hw.acpi.battery.life")?;
            let state = num("hw.acpi.battery.state").unwrap_or(0.0) as u32;
            Some(Battery { percent: percent.clamp(0.0, 100.0) as u32, charging: state & 2 != 0 })
        })
        .flatten();

    Reading {
        volume,
        muted: vol.contains("MUTED"),
        brightness: num("bright"),
        route: v.get("route").copied().unwrap_or("").to_owned(),
        wifi,
        bluetooth: v.get("bt").map(|_| num("btconn").unwrap_or(0.0) as u32),
        battery,
    }
}
