//! The tools the shell runs (wf-recorder, wl-clipboard, cliphist, grim,
//! slurp), installed the first time one is needed, so nobody has to install
//! them by hand. Through eclipse-priv, which installs only these.

use std::process::{Command, Stdio};

use gtk::glib;
use gtk4 as gtk;

const HELPER: &str = "/usr/local/libexec/eclipse-priv";

/// Whether `program` can run, installing `package` first if it's missing.
/// Blocks for the install (seconds), so call it off the main thread.
pub fn ensure(program: &str, package: &str) -> bool {
    if glib::find_program_in_path(program).is_some() {
        return true;
    }
    notify(&format!("Installing {package}"), "Eclipse needs it for this; one moment.");
    let root = if glib::find_program_in_path("doas").is_some() { "doas" } else { "sudo" };
    let installed = Command::new(root).args(["-n", HELPER, "pkg-install", package]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let ok = installed && glib::find_program_in_path(program).is_some();
    if !ok {
        notify(&format!("Couldn't install {package}"), "Run eclipse-shell's install.sh once from a terminal, then try again.");
    }
    ok
}

fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send").args(["-a", "Eclipse", "-i", "system-software-install", title, body]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}
