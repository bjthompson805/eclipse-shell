//! Packages: ">" searches the pkg(8) repository by name (and what's
//! installed). Enter installs, or removes an installed one, in a terminal,
//! since pkg needs root and the password prompt needs a tty; pkg asks to
//! confirm there, so nothing happens on a stray Enter.

use std::collections::HashSet;
use std::process::Command;
use std::rc::Rc;

use super::Cache;
use crate::launcher::results::{Entry, Icon};

const MAX: usize = 40;

#[derive(Clone)]
struct Package {
    name: String,
    version: String,
    comment: String,
    installed: bool,
}

thread_local! {
    static CACHE: Rc<Cache<Result<Vec<Package>, String>>> = Cache::new();
}

pub fn search(query: &str, notify: &Rc<dyn Fn()>) -> (Vec<Entry>, &'static str) {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (Vec::new(), "Type a package name, like >ripgrep");
    }
    // Only names: letters, digits and a few joiners, so it's safe in a regex.
    if !query.chars().all(|c| c.is_ascii_alphanumeric() || "-_+.".contains(c)) {
        return (Vec::new(), "Package names are letters, digits and - _ + .");
    }
    let key = query.clone();
    let found = CACHE.with(|c| c.get(&key, notify, move || find(&query)));
    match found {
        None => (Vec::new(), "Searching packages…"),
        Some(Err(_)) => (Vec::new(), "pkg didn't answer"),
        Some(Ok(list)) => (list.into_iter().map(entry).collect(), "No packages by that name"),
    }
}

/// Forgets results, so an install shows on the next open.
pub fn forget() {
    CACHE.with(|c| c.clear());
}

/// Runs on a worker thread.
fn find(query: &str) -> Result<Vec<Package>, String> {
    let installed: HashSet<String> = Command::new("pkg").args(["query", "%n"]).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_owned).collect()).unwrap_or_default();
    let pattern = query.replace('.', "\\.").replace('+', "\\+");
    let out = Command::new("pkg").args(["rquery", "-x", "%n\t%v\t%c", &pattern]).output().map_err(|e| e.to_string())?;
    let mut list: Vec<Package> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let name = parts.next()?.to_owned();
            Some(Package { installed: installed.contains(&name), version: parts.next()?.to_owned(), comment: parts.next().unwrap_or_default().to_owned(), name })
        })
        .collect();
    list.sort_by_key(|p| {
        let n = p.name.to_lowercase();
        (if n == query { 0 } else if n.starts_with(query) { 1 } else { 2 }, n.len(), n)
    });
    list.dedup_by(|a, b| a.name == b.name);
    list.truncate(MAX);
    Ok(list)
}

fn entry(p: Package) -> Entry {
    let verb = if p.installed { "Remove" } else { "Install" };
    let (name, installed) = (p.name.clone(), p.installed);
    Entry {
        icon: Icon::Glyph(if p.installed { "inventory_2" } else { "package_2" }),
        title: format!("{} {}", p.name, p.version),
        subtitle: if p.installed { format!("Installed · {}", p.comment) } else { p.comment },
        matched: Vec::new(),
        verb,
        action: Rc::new(move || {
            forget();
            let command = format!("{} pkg {} {name}; echo; printf 'Press Enter to close. '; read _", root(), if installed { "remove" } else { "install" });
            in_terminal(&command);
        }),
    }
}

/// doas if it's set up, else sudo.
fn root() -> &'static str {
    if std::path::Path::new("/usr/local/etc/doas.conf").exists() { "doas" } else { "sudo" }
}

/// Runs a shell command line in a terminal window: $TERMINAL, or the
/// first of kitty, foot, alacritty and xterm that's installed.
pub fn in_terminal(command: &str) {
    let installed = |name: &str| std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(name).is_file()));
    let terminal = std::env::var("TERMINAL").ok().filter(|t| installed(t)).or_else(|| ["kitty", "foot", "alacritty", "xterm"].into_iter().find(|t| installed(t)).map(str::to_owned));
    let Some(terminal) = terminal else {
        eprintln!("eclipse-shell: no terminal found to run: {command}");
        return;
    };
    // foot takes the command directly; the others after -e.
    let mut argv = vec![terminal.as_str()];
    if terminal != "foot" {
        argv.push("-e");
    }
    argv.extend(["sh", "-c", command]);
    crate::audio::spawn(&argv);
}
