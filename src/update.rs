//! Keeping the shell current, `eclipse-shell update`. The checkout install.sh
//! last ran from is the channel; what it installed is recorded beside it.
//! System packages are the user's to upgrade: an update never touches them,
//! and install.sh only adds missing ones.
//!
//!   eclipse-shell update status [--json]   commits not yet installed
//!   eclipse-shell update                   boot environment, git pull, install.sh
//!   eclipse-shell update rollback <be>     boot a pre-update boot environment next
//!
//! A run publishes its progress to $XDG_RUNTIME_DIR/eclipse-update.json, which
//! the hub's Updates page shows. It runs in a terminal so sudo can prompt.

use std::path::{Path, PathBuf};
use std::process::Command;

use gtk::glib;
use gtk4 as gtk;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Commit {
    pub subject: String,
    pub hash: String,
}

#[derive(Clone)]
pub struct Status {
    pub branch: String,
    pub installed: String,
    /// What an update would apply.
    pub commits: Vec<Commit>,
}

fn state_dir() -> PathBuf {
    glib::user_state_dir().join("eclipse-shell")
}

/// The checkout updates come from, as install.sh recorded it.
pub fn repo() -> Option<PathBuf> {
    let text = std::fs::read_to_string(state_dir().join("repo")).ok()?;
    Some(PathBuf::from(text.trim())).filter(|r| r.join(".git").exists())
}

/// `git -C repo args`, its output trimmed when it succeeds.
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Fetches origin; blocks (network), so call it off the main thread. Err is
/// the reason, for the page.
pub fn status() -> Result<Status, String> {
    let repo = repo().ok_or("No checkout recorded; run install.sh from one.")?;
    let _ = git(&repo, &["fetch", "-q", "origin"]);
    let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    // What install.sh last installed, not what the checkout has: a pull
    // without an install still leaves those commits to apply.
    let recorded = std::fs::read_to_string(state_dir().join("installed")).unwrap_or_default().trim().to_owned();
    let base = if !recorded.is_empty() && git(&repo, &["cat-file", "-e", &format!("{recorded}^{{commit}}")]).is_some() { recorded } else { "HEAD".to_owned() };
    let installed = git(&repo, &["rev-parse", "--short", &base]).ok_or("The checkout has no commits.")?;
    let commits = if git(&repo, &["rev-parse", "@{u}"]).is_some() {
        git(&repo, &["log", "--format=%h%x09%s", &format!("{base}..@{{u}}")])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(hash, subject)| Commit { subject: subject.to_owned(), hash: hash.to_owned() })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Status { branch, installed, commits })
}

/// A step of a run: snapshot, pull, install; its state is pending, running,
/// ok, skipped or failed.
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
    let text = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
    let mut phase = text(&v, "phase");
    if phase.is_empty() || phase == "idle" {
        return None;
    }
    let mut error = text(&v, "error");
    // A run whose process is gone never finishes: say so, rather than show
    // it running forever.
    let pid = v["pid"].as_i64().and_then(|p| i32::try_from(p).ok());
    // SAFETY: signal 0 only asks whether the process exists.
    if phase == "running" && pid.is_some_and(|p| unsafe { libc::kill(p, 0) } != 0) {
        phase = "error".to_owned();
        error = "The update stopped before it finished.".to_owned();
    }
    let steps = v["steps"].as_array().into_iter().flatten().map(|s| Step { name: text(s, "name"), state: text(s, "state") }).collect();
    let log = v["log"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_owned)).collect();
    Some(Run { phase, label: text(&v, "label"), error, snapshot: text(&v, "snapshot"), steps, log })
}

/// Terminals to run an update in, first found wins, each with the flags
/// that set its class and run a command.
const TERMINALS: &[(&str, &[&str])] = &[
    ("foot", &["--app-id", TERMINAL_CLASS, "--title", "Eclipse update", "--"]),
    ("kitty", &["--class", TERMINAL_CLASS, "--title", "Eclipse update", "--"]),
    ("alacritty", &["--class", TERMINAL_CLASS, "--title", "Eclipse update", "-e"]),
    ("xterm", &["-class", TERMINAL_CLASS, "-T", "Eclipse update", "-e"]),
];

/// The update terminal's class. It floats: tiled, it opened behind the
/// floating hub that started it.
const TERMINAL_CLASS: &str = "eclipse-update";

/// Runs the update, then waits for a key so its output can be read.
const WRAPPER: &str = "\"$0\" update; printf '\\nPress any key to close.'; stty -icanon -echo min 1 2>/dev/null; dd bs=1 count=1 >/dev/null 2>&1";

/// Starts an update in a terminal, where sudo can ask for a password.
pub fn start() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let Some((terminal, flags)) = TERMINALS.iter().find(|(t, _)| glib::find_program_in_path(t).is_some()) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal found (foot, kitty, alacritty or xterm)"));
    };
    let exe = crate::exe()?;
    crate::hyprland::eval(&format!("hl.window_rule({{ name = \"eclipse-update\", match = {{ class = \"^({TERMINAL_CLASS})$\" }}, float = true, size = {{ 1000, 620 }}, center = true }})"));
    Command::new(terminal).args(*flags).args(["sh", "-c", WRAPPER]).arg(exe).process_group(0).stdin(std::process::Stdio::null()).spawn().map(|_| ())
}

fn as_root() -> &'static str {
    if glib::find_program_in_path("doas").is_some() { "doas" } else { "sudo" }
}

/// An update run, publishing as it goes.
struct Progress {
    steps: Vec<(&'static str, &'static str)>,
    log: Vec<String>,
    snapshot: String,
}

impl Progress {
    fn set(&mut self, name: &str, state: &'static str) {
        if let Some(step) = self.steps.iter_mut().find(|(n, _)| *n == name) {
            step.1 = state;
        }
    }

    fn publish(&self, phase: &str, label: &str, error: &str) {
        let steps: Vec<Value> = self.steps.iter().map(|(name, state)| json!({ "name": name, "state": state })).collect();
        let log = &self.log[self.log.len().saturating_sub(6)..];
        let body = json!({ "phase": phase, "label": label, "error": error, "snapshot": self.snapshot, "steps": steps, "log": log, "pid": std::process::id() });
        let file = run_file();
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, body.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }

    fn say(&mut self, text: String) {
        println!("==> {text}");
        self.log.push(text);
    }

    fn fail(&mut self, step: &str, label: &str, error: String) -> glib::ExitCode {
        self.set(step, "failed");
        self.publish("error", label, &error);
        eprintln!("eclipse-shell update: {error}");
        glib::ExitCode::FAILURE
    }
}

fn update() -> glib::ExitCode {
    let Some(repo) = repo() else {
        eprintln!("eclipse-shell update: no checkout recorded; run install.sh from one");
        return glib::ExitCode::FAILURE;
    };
    let mut p = Progress { steps: vec![("snapshot", "pending"), ("pull", "pending"), ("install", "pending")], log: Vec::new(), snapshot: String::new() };

    p.set("snapshot", "running");
    p.publish("running", "Snapshotting", "");
    let be = format!("eclipse-pre-update-{}", glib::DateTime::now_local().and_then(|d| d.format("%Y%m%d-%H%M%S")).map(|s| s.to_string()).unwrap_or_default());
    p.say(format!("creating boot environment {be}"));
    if Command::new(as_root()).args(["bectl", "create", &be]).status().is_ok_and(|s| s.success()) {
        p.snapshot = be;
        p.set("snapshot", "ok");
    } else {
        p.set("snapshot", "skipped");
        p.say("no boot environment (not on ZFS?), continuing".to_owned());
    }

    p.set("pull", "running");
    p.publish("running", "Pulling", "");
    p.say(format!("git pull in {}", repo.display()));
    if !Command::new("git").arg("-C").arg(&repo).args(["pull", "--ff-only"]).status().is_ok_and(|s| s.success()) {
        return p.fail("pull", "Pulling", format!("git pull --ff-only failed in {}", repo.display()));
    }
    p.set("pull", "ok");

    p.set("install", "running");
    p.publish("running", "Installing", "");
    p.say("install.sh".to_owned());
    if !Command::new(repo.join("install.sh")).status().is_ok_and(|s| s.success()) {
        return p.fail("install", "Installing", "install.sh failed".to_owned());
    }
    p.set("install", "ok");

    p.say("done".to_owned());
    // It stays, as a failure does: the hub that opens after the install
    // shows how the last update ended, and the rollback, until the next one.
    p.publish("done", "Update complete", "");
    glib::ExitCode::SUCCESS
}

fn rollback(be: Option<&str>) -> glib::ExitCode {
    let Some(be) = be.filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))) else {
        eprintln!("usage: eclipse-shell update rollback <boot-environment>");
        return glib::ExitCode::FAILURE;
    };
    let listed = Command::new("bectl").args(["list", "-H"]).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split('\t').next() == Some(be))).unwrap_or(false);
    if !listed {
        eprintln!("eclipse-shell update: no boot environment {be}");
        return glib::ExitCode::FAILURE;
    }
    if !Command::new(as_root()).args(["bectl", "activate", be]).status().is_ok_and(|s| s.success()) {
        return glib::ExitCode::FAILURE;
    }
    println!("boot environment {be} is active from the next boot. reboot to roll back.");
    glib::ExitCode::SUCCESS
}

/// `eclipse-shell update ...`, run in the calling process.
pub fn main(args: &[String]) -> glib::ExitCode {
    match args.first().map(String::as_str) {
        None => update(),
        Some("rollback") => rollback(args.get(1).map(String::as_str)),
        Some("status") => match status() {
            Ok(st) if args.get(1).map(String::as_str) == Some("--json") => {
                let commits: Vec<Value> = st.commits.iter().map(|c| json!({ "subject": c.subject, "hash": c.hash })).collect();
                println!("{}", json!({ "branch": st.branch, "installed": st.installed, "commits": commits }));
                glib::ExitCode::SUCCESS
            }
            Ok(st) => {
                println!("branch:    {} @ {}", st.branch, st.installed);
                println!("behind:    {} commit(s)", st.commits.len());
                for c in &st.commits {
                    println!("  {} {}", c.hash, c.subject);
                }
                glib::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("eclipse-shell update: {e}");
                glib::ExitCode::FAILURE
            }
        },
        Some(_) => {
            eprintln!("usage: eclipse-shell update [status [--json] | rollback <boot-environment>]");
            glib::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs() {
        let file = |phase: &str, pid: u32| json!({ "phase": phase, "label": "Installing", "error": "", "snapshot": "", "steps": [], "log": [], "pid": pid }).to_string();
        // Running, and its process alive (this test's own).
        assert_eq!(run(&file("running", std::process::id())).map(|r| r.phase).as_deref(), Some("running"));
        // Running, but its process is gone: it stopped.
        let gone = run(&file("running", 999_999)).unwrap();
        assert_eq!((gone.phase.as_str(), gone.error.as_str()), ("error", "The update stopped before it finished."));
        assert!(run(&file("idle", 1)).is_none());
        assert_eq!(run(&file("done", 999_999)).map(|r| r.phase).as_deref(), Some("done"));
    }
}
