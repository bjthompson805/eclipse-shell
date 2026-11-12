//! System vitals for the bar's stats readout and the resources popout: CPU
//! (overall and per core), memory, temperature, AMD GPU load, disk use and
//! throughput, and while asked for, the busiest processes.
//!
//! One `sysctl` call every two seconds carries the CPU, memory, temperature
//! and GPU counters. Memory counts free and inactive pages plus the ZFS ARC as
//! available, since the ARC gives memory back on demand. Disk throughput is
//! the change in iostat's since-boot totals for the disk holding $HOME; disk
//! use comes from `df` every tenth tick, since it barely moves.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use gtk::{gio, glib};
use gtk4 as gtk;

const POLL: Duration = Duration::from_secs(2);
const DISK_EVERY: u32 = 10;
/// Readings kept for each sparkline.
pub const HISTORY: usize = 44;
const TOP_PROCESSES: usize = 5;
const SYSCTLS: &[&str] = &[
    "kern.cp_times",
    "hw.physmem",
    "hw.pagesize",
    "vm.stats.vm.v_free_count",
    "vm.stats.vm.v_inactive_count",
    "kstat.zfs.misc.arcstats.size",
    "dev.cpu.0.temperature",
    "hw.acpi.thermal.tz0.temperature",
    "sys.device.drmn0.gpu_busy_percent",
    "sys.device.drmn0.vcn_busy_percent",
];

#[derive(Clone, Default)]
pub struct Disk {
    pub percent: u32,
    pub used: String,
    pub total: String,
}

#[derive(Clone, Default)]
pub struct Stats {
    pub cpu: Option<u32>,
    pub cores: Vec<u32>,
    pub mem: Option<u32>,
    pub temp: Option<u32>,
    pub gpu: Option<u32>,
    /// The video engine's load, alongside the GPU's.
    pub video: Option<u32>,
    pub disk: Option<Disk>,
    /// Bytes per second.
    pub disk_read: Option<f64>,
    pub disk_write: Option<f64>,
    pub cpu_history: VecDeque<f64>,
    pub mem_history: VecDeque<f64>,
    pub temp_history: VecDeque<f64>,
    pub gpu_history: VecDeque<f64>,
    /// (CPU %, command), busiest first; filled only while wanted.
    pub processes: Vec<(f64, String)>,
}

thread_local! {
    static STATS: RefCell<Stats> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
    static WANT_PROCESSES: Cell<bool> = const { Cell::new(false) };
}

pub fn current() -> Stats {
    STATS.with(|s| s.borrow().clone())
}

pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

/// Sample the busiest processes too, for as long as someone shows them.
pub fn want_processes(wanted: bool) {
    WANT_PROCESSES.with(|w| w.set(wanted));
    if !wanted {
        STATS.with(|s| s.borrow_mut().processes.clear());
    }
}

pub fn start() {
    glib::spawn_future_local(async {
        let mut prev_times: Vec<u64> = Vec::new();
        let disk_device = disk_device().await;
        let mut prev_io: Option<(f64, f64, Instant)> = None;
        let mut tick = 0u32;
        loop {
            let values = run(&[&["sysctl", "-i"][..], SYSCTLS].concat()).await.unwrap_or_default();
            let io = match &disk_device {
                Some(dev) => run(&["iostat", "-I", "-x", "-d", dev]).await.and_then(|t| parse_iostat(&t, dev)),
                None => None,
            };
            let disk = if tick % DISK_EVERY == 0 { Some(read_disk().await) } else { None };
            let processes = if WANT_PROCESSES.with(Cell::get) { top_processes().await } else { Vec::new() };
            tick = tick.wrapping_add(1);

            STATS.with(|s| {
                let mut s = s.borrow_mut();
                apply_sysctls(&mut s, &values, &mut prev_times);
                if let Some((read_kb, write_kb)) = io {
                    let now = Instant::now();
                    if let Some((r0, w0, at)) = prev_io {
                        let secs = now.duration_since(at).as_secs_f64().max(0.001);
                        s.disk_read = Some(((read_kb - r0) * 1024.0 / secs).max(0.0));
                        s.disk_write = Some(((write_kb - w0) * 1024.0 / secs).max(0.0));
                    }
                    prev_io = Some((read_kb, write_kb, now));
                }
                if let Some(disk) = disk {
                    s.disk = disk;
                }
                s.processes = processes;
            });
            LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
            glib::timeout_future(POLL).await;
        }
    });
}

fn push(history: &mut VecDeque<f64>, value: Option<u32>) {
    if let Some(v) = value {
        if history.len() == HISTORY {
            history.pop_front();
        }
        history.push_back(f64::from(v));
    }
}

fn apply_sysctls(s: &mut Stats, text: &str, prev_times: &mut Vec<u64>) {
    let values: HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once(": ")).map(|(k, v)| (k, v.trim())).collect();
    let num = |key: &str| values.get(key).and_then(|v| v.parse::<f64>().ok());
    let percent = |busy: f64, total: f64| (100.0 * busy / total).round().clamp(0.0, 100.0) as u32;

    // kern.cp_times holds user, nice, sys, intr, idle ticks for every core.
    if let Some(raw) = values.get("kern.cp_times") {
        let times: Vec<u64> = raw.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if times.len() == prev_times.len() && !times.is_empty() {
            let mut cores = Vec::new();
            let (mut all_total, mut all_idle) = (0u64, 0u64);
            for (now, before) in times.chunks(5).zip(prev_times.chunks(5)) {
                let deltas: Vec<u64> = now.iter().zip(before).map(|(n, b)| n.saturating_sub(*b)).collect();
                let total: u64 = deltas.iter().sum();
                let idle = deltas.get(4).copied().unwrap_or(0);
                (all_total, all_idle) = (all_total + total, all_idle + idle);
                cores.push(if total > 0 { percent((total - idle) as f64, total as f64) } else { 0 });
            }
            s.cores = cores;
            s.cpu = (all_total > 0).then(|| percent((all_total - all_idle) as f64, all_total as f64));
        }
        *prev_times = times;
    }

    s.mem = match (num("hw.physmem"), num("hw.pagesize")) {
        (Some(physmem), Some(page)) if physmem > 0.0 => {
            let pages = num("vm.stats.vm.v_free_count").unwrap_or(0.0) + num("vm.stats.vm.v_inactive_count").unwrap_or(0.0);
            let available = pages * page + num("kstat.zfs.misc.arcstats.size").unwrap_or(0.0);
            Some(percent(physmem - available, physmem))
        }
        _ => None,
    };

    // Temperatures read like "39.1C".
    s.temp = ["dev.cpu.0.temperature", "hw.acpi.thermal.tz0.temperature"]
        .iter()
        .find_map(|k| values.get(k).and_then(|v| v.trim_end_matches('C').parse::<f64>().ok()))
        .map(|t| t.round().max(0.0) as u32);
    s.gpu = num("sys.device.drmn0.gpu_busy_percent").map(|g| g.clamp(0.0, 100.0) as u32);
    s.video = num("sys.device.drmn0.vcn_busy_percent").map(|g| g.clamp(0.0, 100.0) as u32);

    let (cpu, mem, temp, gpu) = (s.cpu, s.mem, s.temp, s.gpu);
    push(&mut s.cpu_history, cpu);
    push(&mut s.mem_history, mem);
    push(&mut s.temp_history, temp);
    push(&mut s.gpu_history, gpu);
}

/// The whole disk under $HOME: a UFS device directly, or a ZFS pool's first
/// device, without its partition suffix ("nda0p4" is read as "nda0").
async fn disk_device() -> Option<String> {
    let script = r#"src=$(df "$HOME" | awk 'NR==2{print $1}')
case "$src" in
/dev/*) d=$src ;;
*) d=$(zpool list -vHP "${src%%/*}" 2>/dev/null | awk '$1 ~ /^\/dev\//{print $1; exit}') ;;
esac
d=${d#/dev/}
echo "$d" | sed -E 's/(p[0-9]+|s[0-9]+[a-h]?)$//'"#;
    let device = run(&["sh", "-c", script]).await?.trim().to_owned();
    (!device.is_empty()).then_some(device)
}

/// Since-boot KB read and written: the kr/i and kw/i columns of `iostat -I -x`.
fn parse_iostat(text: &str, device: &str) -> Option<(f64, f64)> {
    let row = text.lines().find(|l| l.split_whitespace().next() == Some(device))?;
    let fields: Vec<f64> = row.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    Some((*fields.get(2)?, *fields.get(3)?))
}

/// Use of the filesystem holding $HOME, as "used of total" in df's units.
async fn read_disk() -> Option<Disk> {
    let text = run(&["sh", "-c", "df -h \"$HOME\" | awk 'NR==2{print $5, $3, $2}'"]).await?;
    let mut fields = text.split_whitespace();
    let percent = fields.next()?.trim_end_matches('%').parse().ok()?;
    Some(Disk { percent, used: fields.next()?.to_owned(), total: fields.next()?.to_owned() })
}

async fn top_processes() -> Vec<(f64, String)> {
    // One "-o" per column: FreeBSD reads anything after "pcpu=" as its header.
    let Some(text) = run(&["ps", "-ax", "-r", "-o", "pcpu=", "-o", "comm="]).await else { return Vec::new() };
    text.lines()
        .filter_map(|l| {
            let (cpu, name) = l.trim_start().split_once(char::is_whitespace)?;
            Some((cpu.parse().ok()?, name.trim().to_owned()))
        })
        // The kernel's idle threads are not work.
        .filter(|(_, name)| name != "idle")
        .take(TOP_PROCESSES)
        .collect()
}

async fn run(argv: &[&str]) -> Option<String> {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    stdout.map(String::from)
}
