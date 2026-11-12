//! Network throughput on the interface that carries the default route, shared
//! by the bar's net-speed readout and the traffic popout.
//!
//! Byte counters are read in-process (getifaddrs) every 1.5 seconds; which
//! interface carries the default route is asked of `route` every 15. Keeps
//! two minutes of rates and the bytes moved since the shell started.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::CStr;
use std::time::{Duration, Instant};

use gtk::{gio, glib};
use gtk4 as gtk;

const TICK: Duration = Duration::from_millis(1500);
const ROUTE_EVERY: u32 = 10;
/// Two minutes of samples.
pub const HISTORY: usize = 80;

#[derive(Clone, Copy, Default)]
pub struct Rate {
    /// Bytes a second.
    pub down: f64,
    pub up: f64,
}

#[derive(Clone, Default)]
pub struct Traffic {
    /// The interface, e.g. "wlan0"; empty with no default route.
    pub iface: String,
    /// The latest rate, None until two readings agree on the interface.
    pub now: Option<Rate>,
    /// Oldest first.
    pub history: VecDeque<Rate>,
    /// Bytes moved on this interface since it came up, and since the shell
    /// started (or since it moved to this interface).
    pub since_boot: (u64, u64),
    pub since_start: (u64, u64),
}

thread_local! {
    static STATE: RefCell<Traffic> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
}

/// The latest rate, without copying the history.
pub fn now() -> Option<Rate> {
    STATE.with(|s| s.borrow().now)
}

pub fn current() -> Traffic {
    STATE.with(|s| s.borrow().clone())
}

/// Calls `f` after every reading.
pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

pub fn start() {
    glib::spawn_future_local(async {
        let mut iface = String::new();
        // The last reading and when, and the counters when this interface was first seen.
        let mut last: Option<(u64, u64, Instant)> = None;
        let mut origin = (0, 0);
        let mut ticks = 0;
        loop {
            if ticks % ROUTE_EVERY == 0 {
                let route = default_route().await;
                if route != iface {
                    iface = route;
                    last = None;
                    STATE.with(|s| *s.borrow_mut() = Traffic { iface: iface.clone(), ..Traffic::default() });
                }
            }
            ticks += 1;

            let reading = (!iface.is_empty()).then(|| counters(&iface)).flatten();
            STATE.with(|s| {
                let mut state = s.borrow_mut();
                let Some((rx, tx)) = reading else {
                    state.now = None;
                    return;
                };
                let at = Instant::now();
                if let Some((prev_rx, prev_tx, prev_at)) = last {
                    let secs = at.duration_since(prev_at).as_secs_f64().max(0.001);
                    let rate = Rate { down: rx.saturating_sub(prev_rx) as f64 / secs, up: tx.saturating_sub(prev_tx) as f64 / secs };
                    state.now = Some(rate);
                    state.history.push_back(rate);
                    while state.history.len() > HISTORY {
                        state.history.pop_front();
                    }
                } else {
                    origin = (rx, tx);
                }
                last = Some((rx, tx, at));
                state.since_boot = (rx, tx);
                state.since_start = (rx.saturating_sub(origin.0), tx.saturating_sub(origin.1));
            });
            LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
            glib::timeout_future(TICK).await;
        }
    });
}

async fn default_route() -> String {
    let Ok(process) = gio::Subprocess::newv(
        &["route", "-n", "get", "default"].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    ) else {
        return String::new();
    };
    let Ok((Some(out), _)) = process.communicate_utf8_future(None).await else { return String::new() };
    out.lines().find_map(|l| l.trim().strip_prefix("interface:")).unwrap_or("").trim().to_owned()
}

/// The interface's received and sent byte counts, from its link-level entry.
fn counters(iface: &str) -> Option<(u64, u64)> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list we own until
    // freeifaddrs; each entry is only read while the list is alive, and
    // ifa_data on an AF_LINK entry points at the interface's if_data.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut entry = list;
        while !entry.is_null() {
            let e = &*entry;
            entry = e.ifa_next;
            if e.ifa_addr.is_null() || i32::from((*e.ifa_addr).sa_family) != libc::AF_LINK || e.ifa_data.is_null() {
                continue;
            }
            if CStr::from_ptr(e.ifa_name).to_bytes() != iface.as_bytes() {
                continue;
            }
            let data = &*(e.ifa_data as *const libc::if_data);
            found = Some((data.ifi_ibytes, data.ifi_obytes));
            break;
        }
        libc::freeifaddrs(list);
        found
    }
}

/// "12.4 KB/s"; `per_second` false leaves off the "/s".
pub fn format_bytes(bytes: f64, per_second: bool) -> String {
    const K: f64 = 1024.0;
    let unit = match bytes {
        b if b < K => format!("{} B", b.round()),
        b if b < K * K => format!("{:.1} KB", b / K),
        b if b < K * K * K => format!("{:.1} MB", b / (K * K)),
        b => format!("{:.1} GB", b / (K * K * K)),
    };
    if per_second { unit + "/s" } else { unit }
}
