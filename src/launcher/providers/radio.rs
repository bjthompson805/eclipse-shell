//! Radio: "@" lists the stations, the one on air first; Enter tunes in (or
//! out). One mpv plays it, without video; its MPRIS plugin, when present,
//! puts it in the bar's now-playing pill. The YouTube stations need yt-dlp,
//! which mpv calls on its own. What's on air is kept in
//! ~/.local/state/eclipse-shell/radio.json.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;

use gtk::glib;
use gtk4 as gtk;

use crate::launcher::results::{Entry, Icon};

/// (id, name, stream)
const STATIONS: &[(&str, &str, &str)] = &[
    ("lofi", "Lofi Girl", "https://www.youtube.com/@LofiGirl/live"),
    ("chillhop", "Chillhop Radio", "https://www.youtube.com/@ChillhopMusic/live"),
    ("groove", "SomaFM Groove Salad", "https://ice1.somafm.com/groovesalad-256-mp3"),
    ("drone", "SomaFM Drone Zone", "https://ice1.somafm.com/dronezone-256-mp3"),
    ("lush", "SomaFM Lush", "https://ice1.somafm.com/lush-128-mp3"),
    ("fluid", "SomaFM Fluid", "https://ice1.somafm.com/fluid-128-mp3"),
    ("secretagent", "SomaFM Secret Agent", "https://ice1.somafm.com/secretagent-128-mp3"),
    ("defcon", "SomaFM DEF CON Radio", "https://ice1.somafm.com/defcon-128-mp3"),
    ("indiepop", "SomaFM Indie Pop Rocks", "https://ice1.somafm.com/indiepop-128-mp3"),
];

/// Where packages put mpv's MPRIS plugin.
const MPRIS: &[&str] = &["/usr/local/lib/mpv/mpris.so", "/usr/local/share/mpv/scripts/mpris.so", "/usr/local/lib/mpv-mpris/mpris.so"];

fn state_file() -> PathBuf {
    glib::user_state_dir().join("eclipse-shell").join("radio.json")
}

/// The station on air, if its mpv is still running.
fn on_air() -> Option<String> {
    let state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(state_file()).ok()?).ok()?;
    let pid = state["pid"].as_i64()?;
    let out = Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).contains("mpv").then(|| state["station"].as_str().map(str::to_owned)).flatten()
}

fn stop() {
    if let Some(pid) = std::fs::read_to_string(state_file()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|s| s["pid"].as_i64()) {
        if on_air().is_some() {
            // SAFETY: signalling a pid just checked to be our radio's mpv.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
    let _ = std::fs::remove_file(state_file());
}

fn start(id: &'static str, name: &'static str, url: &'static str) {
    stop();
    let mut command = Command::new("mpv");
    command.args(["--no-video", "--really-quiet", "--no-terminal", &format!("--force-media-title={name}"), &format!("--title=LIVE · {name}")]);
    if let Some(plugin) = MPRIS.iter().find(|p| std::path::Path::new(p).is_file()) {
        command.arg(format!("--script={plugin}"));
    }
    let Ok(mut child) = command.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        crate::audio::spawn(&["notify-send", "Radio", "mpv isn't installed (run install.sh)"]);
        return;
    };
    let file = state_file();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, serde_json::json!({ "pid": child.id(), "station": id }).to_string());
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

pub fn search(query: &str) -> Vec<Entry> {
    let query = query.trim().to_lowercase();
    let playing = on_air();
    let mut out = Vec::new();
    if let Some(id) = &playing {
        if query.is_empty() || "stop".starts_with(&query) {
            let name = STATIONS.iter().find(|s| s.0 == id).map_or("the radio", |s| s.1);
            out.push(Entry { icon: Icon::Glyph("stop_circle"), title: format!("Stop {name}"), subtitle: "On air".into(), matched: Vec::new(), verb: "Stop", action: Rc::new(stop) });
        }
    }
    for &(id, name, url) in STATIONS {
        if !query.is_empty() && !id.contains(&query) && !name.to_lowercase().contains(&query) {
            continue;
        }
        if playing.as_deref() == Some(id) {
            continue;
        }
        let youtube = url.contains("youtube.com");
        out.push(Entry {
            icon: Icon::Glyph("radio"),
            title: name.to_owned(),
            subtitle: if youtube { "Live radio · YouTube stream, a few seconds to start".into() } else { "Live radio".into() },
            matched: Vec::new(),
            verb: "Play",
            action: Rc::new(move || start(id, name, url)),
        });
    }
    out
}
