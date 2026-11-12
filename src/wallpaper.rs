//! The wallpaper the desktop shows, for surfaces that draw it themselves (the
//! power menu's hero, the overview's workspaces), and changing it.
//!
//! Setting one hands it to awww with a transition, then themes the desktop
//! from it: `wallust run` (rewriting the palette the shell watches, kitty's
//! and Hyprland's colours) and a Hyprland config reload for the borders.
//! Sets run in order on one worker thread; a burst skips to the last.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;

use gtk::{gio, glib};
use gtk4 as gtk;

const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
const TRANSITION_DURATION: &str = "2.2";
const TRANSITION_FPS: &str = "144";
/// awww transitions a set picks from at random.
const TRANSITIONS: &[&[&str]] = &[
    &["--transition-type", "fade", "--transition-bezier", "0.65,0,0.35,1"],
    &["--transition-type", "wipe", "--transition-angle", "30", "--transition-bezier", "0.16,1,0.3,1", "--transition-step", "110"],
    &["--transition-type", "wave", "--transition-angle", "45", "--transition-wave", "25,35", "--transition-bezier", "0.76,0,0.24,1", "--transition-step", "90"],
    &["--transition-type", "center", "--transition-bezier", "0.22,1,0.36,1", "--transition-step", "100"],
    &["--transition-type", "grow", "--transition-pos", "bottom-left", "--transition-bezier", "0.16,1,0.3,1", "--transition-step", "90"],
    &["--transition-type", "outer", "--transition-bezier", "0.65,0,0.35,1", "--transition-step", "90"],
];

/// Where the wallpapers live.
pub fn dir() -> PathBuf {
    glib::home_dir().join("Pictures").join("Wallpapers")
}

/// Every image in [`dir`], by name.
pub fn list() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())))
        .collect();
    out.sort();
    out
}

/// An image's full size, without decoding it.
pub fn size(path: &Path) -> Option<(i32, i32)> {
    gtk::gdk_pixbuf::Pixbuf::file_info(path).map(|(_, w, h)| (w, h))
}

/// The dominant hue (0..360) of decoded BGRA pixels, or None for a mostly
/// grey image: a mean of the hues weighted by how vivid each pixel is.
pub fn hue(data: &[u8]) -> Option<f64> {
    let (mut x, mut y, mut weight, mut count) = (0.0, 0.0, 0.0, 0.0);
    for p in data.chunks_exact(4).step_by(3) {
        let (b, g, r) = (f64::from(p[0]) / 255.0, f64::from(p[1]) / 255.0, f64::from(p[2]) / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let delta = max - min;
        count += 1.0;
        if delta < 0.04 || max < 0.12 {
            continue;
        }
        let h = if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        let w = (delta / max) * max;
        let rad = h.to_radians();
        x += rad.cos() * w;
        y += rad.sin() * w;
        weight += w;
    }
    // Too little colour overall to call it anything but grey.
    if count == 0.0 || weight / count < 0.08 {
        return None;
    }
    Some(y.atan2(x).to_degrees().rem_euclid(360.0))
}

fn state_dir() -> PathBuf {
    glib::user_state_dir().join("eclipse-shell")
}

/// Shows `path` and themes the desktop from it.
pub fn set(path: &Path) {
    static WORKER: OnceLock<mpsc::Sender<PathBuf>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        std::thread::spawn(move || {
            while let Ok(mut path) = rx.recv() {
                // A burst of picks: only the last one matters.
                while let Ok(newer) = rx.try_recv() {
                    path = newer;
                }
                apply(&path);
            }
        });
        tx
    });
    let _ = worker.send(path.to_owned());
}

/// Runs on the worker thread.
fn apply(path: &Path) {
    let transition = TRANSITIONS[glib::random_int_range(0, TRANSITIONS.len() as i32) as usize];
    let shown = Command::new("awww")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg("img")
        .arg(path)
        .args(transition)
        .args(["--transition-duration", TRANSITION_DURATION, "--transition-fps", TRANSITION_FPS])
        .status()
        .is_ok_and(|s| s.success());
    if !shown {
        eprintln!("eclipse-shell: awww could not show {}", path.display());
        return;
    }
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(state_dir().join("wallpaper"), format!("{}\n", path.display()));
    let quiet = |program: &str| {
        let mut command = Command::new(program);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        command
    };
    let _ = quiet("wallust").arg("run").arg(path).args(tune(path)).status();
    let _ = quiet("hyprctl").args(["reload", "config-only"]).status();
}

/// Extra `wallust run` flags saved for this very image (Ryoku's wallhaven
/// app wrote them); other images use the wallust config's defaults.
fn tune(path: &Path) -> Vec<String> {
    let file = glib::user_state_dir().join("ryoku-wallust.json");
    let Some(t) = std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return Vec::new() };
    if t["image"].as_str() != path.to_str() {
        return Vec::new();
    }
    // Enum values are short lowercase words; anything else is ignored.
    let name = |key: &str| t[key].as_str().filter(|v| !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())).map(str::to_owned);
    let percent = |key: &str| t[key].as_i64().filter(|v| (1..=100).contains(v)).map(|v| v.to_string());
    let mut args = Vec::new();
    for (flag, value) in [("-p", name("palette")), ("-c", name("colorspace")), ("-b", name("backend")), ("--saturation", percent("saturation")), ("-t", percent("threshold"))] {
        if let Some(v) = value {
            args.push(flag.to_owned());
            args.push(v);
        }
    }
    if t["contrast"].as_bool() == Some(true) {
        args.push("-k".to_owned());
    }
    args
}

/// Moves on to the next wallpaper from a shuffled bag, so each shows once
/// per round.
pub fn next() {
    let bag_file = state_dir().join("wallpaper-bag");
    let current = std::fs::read_to_string(state_dir().join("wallpaper")).unwrap_or_default().trim().to_owned();
    let mut bag: Vec<PathBuf> = std::fs::read_to_string(&bag_file).unwrap_or_default().lines().map(PathBuf::from).filter(|p| p.is_file()).collect();
    if bag.is_empty() {
        bag = list();
        for i in (1..bag.len()).rev() {
            bag.swap(i, glib::random_int_range(0, i as i32 + 1) as usize);
        }
        // Don't start the new round with what's showing.
        if bag.len() > 1 && bag[0].to_str() == Some(current.as_str()) {
            let first = bag.remove(0);
            bag.push(first);
        }
    }
    if bag.is_empty() {
        return;
    }
    let pick = bag.remove(0);
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(&bag_file, bag.iter().map(|p| format!("{}\n", p.display())).collect::<String>());
    set(&pick);
}

/// Decodes an image scaled to `height` device pixels tall, as cairo ARGB32
/// (premultiplied BGRA) rows. Runs on a worker thread.
pub fn decode_scaled(path: &str, height: i32) -> Option<(Vec<u8>, i32, i32)> {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, -1, height, true).ok()?;
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let (stride, channels) = (pixbuf.rowstride() as usize, pixbuf.n_channels() as usize);
    let bytes = pixbuf.read_pixel_bytes();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let row = &bytes[y * stride..];
        for x in 0..w as usize {
            let p = &row[x * channels..];
            let a = if channels == 4 { u32::from(p[3]) } else { 255 };
            let pre = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
            out.extend_from_slice(&[pre(p[2]), pre(p[1]), pre(p[0]), a as u8]);
        }
    }
    Some((out, w, h))
}

/// The image the wallpaper daemon (awww) is showing.
pub async fn current() -> Option<String> {
    let process = gio::Subprocess::newv(&["awww", "query"].map(std::ffi::OsStr::new), gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    // ": eDP-1: 1920x1200, scale: 1.5, currently displaying: image: /path"
    stdout?.lines().find_map(|l| l.split("image: ").nth(1)).map(|p| p.trim().to_owned())
}
