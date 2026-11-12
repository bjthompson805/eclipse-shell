//! Minimal Hyprland IPC: one-shot requests on `.socket.sock` and the event
//! stream on `.socket2.sock`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub workspace: i32,
    pub class: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// Active workspace per monitor connector, e.g. "eDP-1".
    pub active: HashMap<String, i32>,
    /// Mapped windows on regular (positive id) workspaces.
    pub windows: Vec<Window>,
    pub focused: Option<Window>,
    /// Connector of the monitor with keyboard focus.
    pub focused_monitor: Option<String>,
}

#[derive(Deserialize)]
struct RawMonitor {
    name: String,
    #[serde(default)]
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    active_workspace: WorkspaceRef,
}

#[derive(Deserialize)]
struct WorkspaceRef {
    id: i32,
}

#[derive(Deserialize)]
struct RawClient {
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    workspace: Option<WorkspaceRef>,
}

impl RawClient {
    fn into_window(self) -> Option<Window> {
        let workspace = self.workspace?.id;
        Some(Window { workspace, class: self.class, title: self.title })
    }
}

fn socket_dir() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    match (runtime, signature) {
        (Some(runtime), Some(signature)) => Ok(PathBuf::from(runtime).join("hypr").join(signature)),
        _ => Err(io::Error::new(io::ErrorKind::NotFound, "not running under Hyprland")),
    }
}

pub fn request(command: &str) -> io::Result<String> {
    let mut stream = UnixStream::connect(socket_dir()?.join(".socket.sock"))?;
    stream.write_all(command.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

/// Runs a request that answers "ok" on success, logging anything else.
fn command(what: &str, command: &str) {
    match request(command) {
        Ok(reply) if reply.trim() == "ok" => {}
        Ok(reply) => eprintln!("eclipse-shell: {what} failed: {}", reply.trim()),
        Err(err) => eprintln!("eclipse-shell: {what} failed: {err}"),
    }
}

/// Hyprland with a Lua config wraps dispatch arguments in `hl.dispatch(...)`,
/// so the argument is a Lua dispatcher call rather than the classic syntax.
pub fn focus_workspace(id: i32) {
    // Pull the workspace onto this monitor first, so a click always lands here.
    command("moving a workspace", &format!("dispatch hl.dsp.workspace.move({{ workspace = {id}, monitor = \"current\" }})"));
    command("switching workspace", &format!("dispatch hl.dsp.focus({{ workspace = {id} }})"));
}

/// Evaluates Lua against the running Hyprland config.
pub fn eval(lua: &str) {
    command("hyprland eval", &format!("eval {lua}"));
}

pub fn option_json(name: &str) -> Option<serde_json::Value> {
    serde_json::from_str(&request(&format!("j/getoption {name}")).ok()?).ok()
}

pub fn query_state() -> io::Result<State> {
    let monitors: Vec<RawMonitor> = serde_json::from_str(&request("j/monitors")?)?;
    let clients: Vec<RawClient> = serde_json::from_str(&request("j/clients")?)?;
    // An empty desktop answers "{}", which has no workspace and maps to None.
    let focused = serde_json::from_str::<RawClient>(&request("j/activewindow")?).ok().and_then(RawClient::into_window);

    let focused_monitor = monitors.iter().find(|m| m.focused).map(|m| m.name.clone());
    Ok(State {
        focused_monitor,
        active: monitors.into_iter().map(|m| (m.name, m.active_workspace.id)).collect(),
        // Negative ids are special (scratchpad) workspaces; leave them off the bar.
        windows: clients.into_iter().filter_map(RawClient::into_window).filter(|w| w.workspace > 0).collect(),
        focused,
    })
}

/// Events after which the bar may need to change.
const REFRESH_EVENTS: &[&str] = &[
    "workspace",
    "createworkspace",
    "destroyworkspace",
    "moveworkspace",
    "renameworkspace",
    "focusedmon",
    "monitoradded",
    "monitorremoved",
    "openwindow",
    "closewindow",
    "movewindow",
    "activewindow",
    "windowtitle",
];

/// How long to wait for more events before querying, so bursts (a window
/// opening fires several) cost one refresh.
const SETTLE: Duration = Duration::from_millis(30);

/// Sends the current state, then a fresh copy whenever it changes.
/// The event stream is read on its own thread so the UI never blocks on it.
pub fn watch() -> async_channel::Receiver<State> {
    let (tx, rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        let stream = match socket_dir().and_then(|dir| UnixStream::connect(dir.join(".socket2.sock"))) {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("eclipse-shell: cannot listen for Hyprland events: {err}");
                return;
            }
        };

        let mut last: Option<State> = None;
        let mut publish = || match query_state() {
            Ok(state) if last.as_ref() != Some(&state) => {
                last = Some(state.clone());
                tx.send_blocking(state).is_ok()
            }
            Ok(_) => true,
            Err(err) => {
                eprintln!("eclipse-shell: Hyprland query failed: {err}");
                true
            }
        };
        if !publish() {
            return;
        }

        let timeout_handle = stream.try_clone();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let mut pending = false;
        loop {
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    // Events look like "name>>data"; v2 variants share the prefix.
                    let name = line.split(">>").next().unwrap_or_default();
                    if REFRESH_EVENTS.iter().any(|e| name.starts_with(e)) && !pending {
                        pending = true;
                        if let Ok(s) = &timeout_handle {
                            let _ = s.set_read_timeout(Some(SETTLE));
                        }
                    }
                    line.clear();
                }
                // The settle window passed with no more events. Any partial
                // line stays in `line` and is completed by the next read.
                Err(err) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    pending = false;
                    if let Ok(s) = &timeout_handle {
                        let _ = s.set_read_timeout(None);
                    }
                    if !publish() {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("eclipse-shell: Hyprland event stream closed: {err}");
                    break;
                }
            }
        }
    });
    rx
}
