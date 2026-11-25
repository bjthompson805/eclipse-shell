//! What the Updates page shows: `eclipse-update status --json` (commits the
//! checkout is behind) and the progress an update run publishes to
//! $XDG_RUNTIME_DIR/eclipse-update.json. System packages are the user's to
//! upgrade; an update never touches them.

use std::path::PathBuf;
use std::process::Command;

use gtk::glib;
use gtk4 as gtk;
use serde_json::Value;

#[derive(Clone)]
pub struct Commit {
    pub subject: String,
    pub hash: String,
}

#[derive(Clone)]
pub struct Status {
    pub branch: String,
    pub installed: String,
    /// What a pull would bring in.
    pub commits: Vec<Commit>,
    pub recent: Vec<Commit>,
}

fn commits(v: &Value) -> Vec<Commit> {
    let text = |i: &Value, k: &str| i[k].as_str().unwrap_or_default().to_owned();
    v.as_array().into_iter().flatten().map(|i| Commit { subject: text(i, "subject"), hash: text(i, "hash") }).collect()
}

fn script() -> PathBuf {
    glib::home_dir().join(".local/bin/eclipse-update")
}

/// The checkout updates come from, as install.sh recorded it.
pub fn repo() -> Option<String> {
    let text = std::fs::read_to_string(glib::user_state_dir().join("eclipse-shell/repo")).ok()?;
    Some(text.trim().to_owned()).filter(|r| !r.is_empty())
}

/// Fetches origin; blocks (network), so call it off the main
/// thread. Err is the reason, for the page.
pub fn check() -> Result<Status, String> {
    let out = Command::new(script()).args(["status", "--json"]).output().map_err(|e| format!("Couldn't run eclipse-update: {e}. Run install.sh again."))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().trim_start_matches("eclipse-update: ").to_owned();
        return Err(if err.is_empty() { "The check failed.".to_owned() } else { err });
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "eclipse-update gave no status.".to_owned())?;
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    Ok(Status { branch: text("branch"), installed: text("installed"), commits: commits(&v["commits"]), recent: commits(&v["recent"]) })
}

/// A step of a run: snapshot, pull, install; its state is
/// pending, running, ok, skipped or failed.
pub struct Step {
    pub name: String,
    pub state: String,
}

pub struct Run {
    /// running, error, done or idle.
    pub phase: String,
    pub label: String,
    pub error: String,
    /// The boot environment made before the update, if any.
    pub snapshot: String,
    pub steps: Vec<Step>,
    pub log: Vec<String>,
}

fn run_file() -> PathBuf {
    glib::user_runtime_dir().join("eclipse-update.json")
}

/// The run file's text, to tell when it changes.
pub fn run_text() -> String {
    std::fs::read_to_string(run_file()).unwrap_or_default()
}

/// The last run, unless none ran or it has gone back to idle.
pub fn run(text: &str) -> Option<Run> {
    let v: Value = serde_json::from_str(text).ok()?;
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    let phase = text("phase");
    if phase.is_empty() || phase == "idle" {
        return None;
    }
    let steps = v["steps"].as_array().into_iter().flatten().map(|s| Step { name: s["name"].as_str().unwrap_or_default().to_owned(), state: s["state"].as_str().unwrap_or_default().to_owned() }).collect();
    let log = v["log"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_owned)).collect();
    Some(Run { phase, label: text("label"), error: text("error"), snapshot: text("snapshot"), steps, log })
}

/// Starts an update in a terminal, where sudo can ask for a password.
pub fn start() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let script = script();
    Command::new("foot")
        .args(["--title", "Eclipse update", "--hold", "--"])
        .arg(script)
        .arg("update")
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}
