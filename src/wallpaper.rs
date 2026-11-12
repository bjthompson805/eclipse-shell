//! The wallpaper the desktop shows, for surfaces that draw it themselves (the
//! power menu's hero, the overview's workspaces), and changing it.
//!
//! Setting one hands it to awww with a transition, then themes the desktop
//! from it: `wallust run` (rewriting the palette the shell watches, kitty's
//! and Hyprland's colours) and a Hyprland config reload for the borders.
//! Sets run in order on one worker thread; a burst skips to the last.
//!
//! Live wallpapers are clips in ~/Pictures/livewalls. awww shows the clip's
//! first frame (its poster, cached) and a `eclipse-shell livewall` process
//! plays the clip over it (see [`crate::livewall`]); themes come from the
//! poster. What's set is recorded in ~/.local/state/eclipse-shell/wallpaper.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;

use gtk::{gio, glib};
use gtk4 as gtk;

use crate::hyprland;

const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mkv", "mov"];
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

/// Where the live wallpapers live.
pub fn live_dir() -> PathBuf {
    glib::home_dir().join("Pictures").join("livewalls")
}

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| list.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn is_video(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

/// Every image in [`dir`], then every clip in [`live_dir`], each by name.
pub fn list() -> Vec<PathBuf> {
    let read = |dir: PathBuf, kinds: &[&str]| {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| has_extension(p, kinds)).collect();
        out.sort();
        out
    };
    let mut all = read(dir(), EXTENSIONS);
    all.extend(read(live_dir(), VIDEO_EXTENSIONS));
    all
}

/// A wallpaper's full size, without decoding it.
pub fn size(path: &Path) -> Option<(i32, i32)> {
    if is_video(path) {
        return crate::livewall::probe(path).map(|(w, h, _)| (w as i32, h as i32));
    }
    gtk::gdk_pixbuf::Pixbuf::file_info(path).map(|(_, w, h)| (w, h))
}

/// A still of a clip's first frame, made once and cached by path and mtime.
/// Runs ffmpeg, so call it off the main thread.
pub fn poster(clip: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mtime = std::fs::metadata(clip).and_then(|m| m.modified()).ok();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (clip, mtime).hash(&mut hasher);
    let cache = glib::user_cache_dir().join("eclipse-shell").join("posters");
    let out = cache.join(format!("{:016x}.png", hasher.finish()));
    if out.is_file() {
        return Some(out);
    }
    std::fs::create_dir_all(&cache).ok()?;
    let made = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(clip)
        .args(["-frames:v", "1"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    made.then_some(out)
}

/// The image to draw or theme from: the poster for a clip, else the path.
pub fn still(path: &Path) -> Option<PathBuf> {
    if is_video(path) { poster(path) } else { Some(path.to_owned()) }
}

/// What's set, as recorded when it was set.
pub fn recorded() -> Option<PathBuf> {
    let text = std::fs::read_to_string(state_dir().join("wallpaper")).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_file().then_some(path)
}

fn livewall_pid_file() -> PathBuf {
    state_dir().join("livewall.pid")
}

/// The running live wallpaper process, checked by name so a reused pid
/// is never mistaken for it.
fn livewall_pid() -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(livewall_pid_file()).ok()?.trim().parse().ok()?;
    let out = Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).contains("livewall").then_some(pid)
}

fn stop_livewall() {
    if let Some(pid) = livewall_pid() {
        // SAFETY: signalling a pid just checked to be our live wallpaper.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let _ = std::fs::remove_file(livewall_pid_file());
}

fn start_livewall(clip: &Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    let Ok(mut child) = Command::new(exe).arg("livewall").arg(clip).stdin(Stdio::null()).stdout(Stdio::null()).spawn() else { return };
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(livewall_pid_file(), child.id().to_string());
    // Reap it when it ends, so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// At startup: plays the live wallpaper again if one was set and nothing
/// plays it. awww restores the still by itself.
pub fn resume() {
    hyprland::keep(|| hyprland::eval(&format!("hl.layer_rule({{ name = \"{0}-fade\", match = {{ namespace = \"^{0}$\" }}, animation = \"fade\" }})", crate::livewall::NAMESPACE)));
    std::thread::spawn(|| {
        if let Some(clip) = recorded().filter(|p| is_video(p)) {
            if livewall_pid().is_none() {
                start_livewall(&clip);
            }
        }
    });
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
    set_with(path, true);
}

/// Shows `path`; with `theme` false the colours stay as they are.
pub fn set_with(path: &Path, theme: bool) {
    static WORKER: OnceLock<mpsc::Sender<(PathBuf, bool)>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(PathBuf, bool)>();
        std::thread::spawn(move || {
            while let Ok(mut job) = rx.recv() {
                // A burst of picks: only the last one matters.
                while let Ok(newer) = rx.try_recv() {
                    job = newer;
                }
                apply(&job.0, job.1);
            }
        });
        tx
    });
    let _ = worker.send((path.to_owned(), theme));
}

/// Runs on the worker thread.
fn apply(path: &Path, theme: bool) {
    let Some(still) = still(path) else {
        eprintln!("eclipse-shell: no still for {}", path.display());
        return;
    };
    let transition = TRANSITIONS[glib::random_int_range(0, TRANSITIONS.len() as i32) as usize];
    let shown = Command::new("awww")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg("img")
        .arg(&still)
        .args(transition)
        .args(["--transition-duration", TRANSITION_DURATION, "--transition-fps", TRANSITION_FPS])
        .status()
        .is_ok_and(|s| s.success());
    if !shown {
        eprintln!("eclipse-shell: awww could not show {}", path.display());
        return;
    }
    stop_livewall();
    if is_video(path) {
        start_livewall(path);
    }
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(state_dir().join("wallpaper"), format!("{}\n", path.display()));
    if !theme {
        return;
    }
    let quiet = |program: &str| {
        let mut command = Command::new(program);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        command
    };
    let _ = quiet("wallust").arg("run").arg(&still).args(tune(path)).status();
    let _ = quiet("hyprctl").args(["reload", "config-only"]).status();
}

fn tunes_file() -> PathBuf {
    state_dir().join("wallust-tunes.json")
}

/// Every saved tune, by wallpaper path.
pub fn tunes() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(tunes_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Saves (or with None, forgets) the tune for `path`: palette, colorspace,
/// backend, saturation, threshold, contrast.
pub fn save_tune(path: &Path, tune: Option<serde_json::Value>) {
    let mut all = tunes();
    let key = path.display().to_string();
    match tune {
        Some(t) => all.insert(key, t),
        None => all.remove(&key),
    };
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(tunes_file(), serde_json::to_string_pretty(&all).unwrap_or_default());
}

/// Extra `wallust run` flags saved for this wallpaper; others use the wallust
/// config's defaults.
pub fn tune(path: &Path) -> Vec<String> {
    let Some(t) = tunes().get(&path.display().to_string()).cloned() else { return Vec::new() };
    tune_args(&t)
}

pub fn tune_args(t: &serde_json::Value) -> Vec<String> {
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
    let current = recorded().map(|p| p.display().to_string()).unwrap_or_default();
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

/// The image on screen: a clip's poster while a live wallpaper plays.
pub async fn current() -> Option<String> {
    let process = gio::Subprocess::newv(&["awww", "query"].map(std::ffi::OsStr::new), gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    // ": eDP-1: 1920x1200, scale: 1.5, currently displaying: image: /path"
    stdout?.lines().find_map(|l| l.split("image: ").nth(1)).map(|p| p.trim().to_owned())
}
