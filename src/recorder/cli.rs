//! `eclipse-shell record`, run in the calling process:
//!
//!   eclipse-shell record [--region [WxH+X+Y]] [--desktop-audio] [--mic]
//!   eclipse-shell record stop | status | info
//!
//! Start records the focused monitor, or a region (slurp picks one when none
//! is given). wf-recorder runs in a process group of its own, so it outlives
//! this command and whoever ran it. Settings come from the recorder's
//! settings (Eclipse Recorder; env vars override). wf-recorder takes one
//! audio device, so desktop audio wins when both are asked for.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use gtk::glib;
use gtk4 as gtk;

use super::{recordings_dir, settings, valid_geometry, Settings};

/// Whether wf-recorder is running.
pub fn running() -> bool {
    Command::new("pgrep").args(["-x", "wf-recorder"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn notify(args: &[&str]) {
    let _ = Command::new("notify-send").arg("-a").arg("Eclipse Recorder").args(args).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn render_node() -> Option<String> {
    if let Some(d) = std::env::var("ECLIPSE_SCREENRECORD_DEVICE").ok().filter(|d| !d.is_empty()) {
        return Some(d);
    }
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri").into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("renderD"))).collect();
    nodes.sort();
    nodes.first().map(|p| p.display().to_string())
}

/// The hardware and software encoders for a codec.
fn hw_codec(codec: &str) -> &'static str {
    match codec {
        "hevc" => "hevc_vaapi",
        "av1" => "av1_vaapi",
        _ => "h264_vaapi",
    }
}

fn sw_codec(codec: &str) -> &'static str {
    match codec {
        "hevc" => "libx265",
        "av1" => "libsvtav1",
        _ => "libx264",
    }
}

/// Quality as VAAPI's qp and the software crf (lower is crisper); SVT-AV1's
/// crf scale runs higher for the same look.
fn qp_for(quality: &str) -> u32 {
    match quality {
        "ultra" => 16,
        "very_high" => 20,
        "high" => 24,
        _ => 28,
    }
}

fn crf_for(quality: &str, codec: &str) -> u32 {
    let c = match quality {
        "ultra" => 17,
        "very_high" => 20,
        "high" => 23,
        _ => 28,
    };
    if codec == "av1" { c + 10 } else { c }
}

fn focused_output() -> Option<String> {
    let out = Command::new("hyprctl").args(["monitors", "-j"]).stderr(Stdio::null()).output().ok()?;
    let monitors: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    let name = |m: &serde_json::Value| m["name"].as_str().map(str::to_owned);
    monitors.iter().find(|m| m["focused"].as_bool() == Some(true)).and_then(name).or_else(|| monitors.first().and_then(name))
}

/// The region as wf-recorder's "X,Y WxH": the box given, else slurp's.
fn region(geometry: Option<&str>) -> Option<String> {
    if let Some(g) = geometry.filter(|g| valid_geometry(g)) {
        let (size, pos) = g.split_once('+')?;
        let (x, y) = pos.split_once('+')?;
        return Some(format!("{x},{y} {size}"));
    }
    let out = Command::new("slurp").stderr(Stdio::null()).output().ok()?;
    let g = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !g.is_empty()).then_some(g)
}

fn next_path() -> PathBuf {
    let stamp = glib::DateTime::now_local().and_then(|d| d.format("%Y-%m-%d_%H.%M.%S")).map(|s| s.to_string()).unwrap_or_default();
    let base = recordings_dir().join(format!("recording_{stamp}"));
    let mut path = base.with_extension("mp4");
    let mut i = 1;
    while path.exists() {
        path = PathBuf::from(format!("{}-{i}.mp4", base.display()));
        i += 1;
    }
    path
}

fn pactl(what: &str) -> Option<String> {
    let out = Command::new("pactl").arg(what).stderr(Stdio::null()).output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// wf-recorder's output, kept for when a recording fails.
fn log_file() -> Option<std::fs::File> {
    let dir = glib::user_state_dir().join("eclipse-shell");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::File::create(dir.join("wf-recorder.log")).ok()
}

fn spawn(args: &[String]) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let log = log_file();
    let (out, err) = match log.as_ref().and_then(|f| Some((f.try_clone().ok()?, f.try_clone().ok()?))) {
        Some((a, b)) => (Stdio::from(a), Stdio::from(b)),
        None => (Stdio::null(), Stdio::null()),
    };
    Command::new("wf-recorder").args(args).process_group(0).stdin(Stdio::null()).stdout(out).stderr(err).spawn()
}

fn nonempty(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

struct Start {
    region: bool,
    geometry: Option<String>,
    desktop_audio: bool,
    mic: bool,
}

fn start(opts: Start) -> glib::ExitCode {
    if running() {
        notify(&["Screen recording", "A recording is already running"]);
        return glib::ExitCode::SUCCESS;
    }
    // Installed on first use; ensure() says why if it can't be.
    if !crate::deps::ensure("wf-recorder", "wf-recorder") {
        eprintln!("eclipse-shell record: wf-recorder is not installed");
        return glib::ExitCode::FAILURE;
    }
    let Settings { fps, quality, codec, encoder } = settings();

    let mut args = vec!["-r".to_owned(), fps.to_string()];
    if opts.region {
        let Some(g) = region(opts.geometry.as_deref()) else { return glib::ExitCode::SUCCESS };
        args.extend(["-g".to_owned(), g]);
    } else if let Some(out) = focused_output() {
        args.extend(["-o".to_owned(), out]);
    }
    if opts.desktop_audio || opts.mic {
        let dev = if opts.desktop_audio { pactl("get-default-sink").map(|s| format!("{s}.monitor")) } else { pactl("get-default-source") };
        args.push(dev.map_or_else(|| "--audio".to_owned(), |d| format!("--audio={d}")));
    }

    let _ = std::fs::create_dir_all(recordings_dir());
    let file = next_path();
    let file_arg = file.display().to_string();

    // VAAPI first. If the encoder can't come up it exits within a moment, and
    // the recording starts again on the CPU encoder, so a file always lands.
    if encoder == "gpu"
        && let Some(node) = render_node()
    {
        let mut hw = args.clone();
        hw.extend(["-c".into(), hw_codec(&codec).into(), "-d".into(), node, "-p".into(), format!("qp={}", qp_for(&quality)), "-F".into(), "scale_vaapi=format=nv12".into(), "-f".into(), file_arg.clone()]);
        if let Ok(mut child) = spawn(&hw) {
            std::thread::sleep(Duration::from_millis(1500));
            if matches!(child.try_wait(), Ok(None)) || nonempty(&file) {
                return glib::ExitCode::SUCCESS;
            }
            let _ = std::fs::remove_file(&file);
        }
    }
    let mut sw = args;
    sw.extend(["-c".into(), sw_codec(&codec).into(), "-p".into(), format!("crf={}", crf_for(&quality, &codec)), "-f".into(), file_arg]);
    match spawn(&sw) {
        Ok(_) => glib::ExitCode::SUCCESS,
        Err(e) => {
            notify(&["-u", "critical", "Screen recording failed", &e.to_string()]);
            glib::ExitCode::FAILURE
        }
    }
}

fn stop() -> glib::ExitCode {
    if !running() {
        notify(&["Screen recording", "No active recording found"]);
        return glib::ExitCode::SUCCESS;
    }
    let _ = Command::new("pkill").args(["-INT", "-x", "wf-recorder"]).status();
    notify(&["Screen recording stopped", "Saved to Videos/Recordings"]);
    glib::ExitCode::SUCCESS
}

/// The encoder a recording would use on this machine.
pub fn encoder_info() -> String {
    let Settings { codec, encoder, .. } = settings();
    match render_node().filter(|_| encoder == "gpu") {
        Some(node) => format!("VAAPI {} on {node}", hw_codec(&codec)),
        None => format!("CPU {}", sw_codec(&codec)),
    }
}

pub fn main(args: &[String]) -> glib::ExitCode {
    match args.first().map(String::as_str) {
        Some("stop") => return stop(),
        Some("status") => {
            println!("{}", if running() { "recording" } else { "idle" });
            return glib::ExitCode::SUCCESS;
        }
        Some("info") => {
            println!("{}", serde_json::json!({ "backend": "wf", "encoder": encoder_info() }));
            return glib::ExitCode::SUCCESS;
        }
        _ => {}
    }
    let mut opts = Start { region: false, geometry: None, desktop_audio: false, mic: false };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--region" => {
                opts.region = true;
                if args.get(i + 1).is_some_and(|g| valid_geometry(g)) {
                    i += 1;
                    opts.geometry = Some(args[i].clone());
                }
            }
            "--desktop-audio" => opts.desktop_audio = true,
            "--mic" => opts.mic = true,
            other => {
                eprintln!("eclipse-shell record: unknown argument: {other}");
                eprintln!("usage: eclipse-shell record [--region [WxH+X+Y]] [--desktop-audio] [--mic] | stop | status | info");
                return glib::ExitCode::FAILURE;
            }
        }
        i += 1;
    }
    start(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_and_presets() {
        assert_eq!(region(Some("800x600+10+20")).as_deref(), Some("10,20 800x600"));
        assert_eq!(qp_for("ultra"), 16);
        assert_eq!(crf_for("high", "av1"), 33);
        assert_eq!(hw_codec("hevc"), "hevc_vaapi");
        assert_eq!(sw_codec("x"), "libx264");
        assert!(!valid_geometry("800x600"));
    }
}
