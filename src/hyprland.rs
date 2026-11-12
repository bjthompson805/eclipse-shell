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
    /// Hyprland's handle for the window, stable while it lives.
    pub address: String,
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
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    workspace: Option<WorkspaceRef>,
}

impl RawClient {
    fn into_window(self) -> Option<Window> {
        let workspace = self.workspace?.id;
        Some(Window { address: self.address, workspace, class: self.class, title: self.title })
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

/// Focuses a window, switching to its workspace.
pub fn focus_window(address: &str) {
    command("focusing a window", &format!("dispatch hl.dsp.focus({{ window = \"address:{address}\" }})"));
}

/// Moves a window to a workspace, staying where we are.
pub fn move_window(address: &str, workspace: i32) {
    command("moving a window", &format!("dispatch hl.dsp.window.move({{ workspace = {workspace}, window = \"address:{address}\" }})"));
}

/// Asks a window to close, as its own close button would.
pub fn close_window(address: &str) {
    command("closing a window", &format!("dispatch hl.dsp.window.close({{ window = \"address:{address}\" }})"));
}

/// Where everything sits, for drawing workspaces as maps.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub monitors: Vec<Screen>,
    /// Regular workspaces (positive ids), lowest first.
    pub workspaces: Vec<Space>,
    pub windows: Vec<Placed>,
}

#[derive(Clone, Debug)]
pub struct Screen {
    pub name: String,
    /// Logical px, as window positions are given.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub active: i32,
}

#[derive(Clone, Debug)]
pub struct Space {
    pub id: i32,
    pub monitor: String,
}

#[derive(Clone, Debug)]
pub struct Placed {
    pub address: String,
    pub workspace: i32,
    pub class: String,
    pub title: String,
    /// Logical px on the whole desktop.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub floating: bool,
}

#[derive(Deserialize)]
struct LayoutMonitor {
    name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    #[serde(default)]
    transform: i32,
    #[serde(rename = "activeWorkspace")]
    active_workspace: WorkspaceRef,
}

#[derive(Deserialize)]
struct LayoutWorkspace {
    id: i32,
    monitor: String,
}

#[derive(Deserialize)]
struct LayoutClient {
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    workspace: WorkspaceRef,
    at: [f64; 2],
    size: [f64; 2],
    #[serde(default)]
    floating: bool,
    #[serde(default = "yes")]
    mapped: bool,
}

fn yes() -> bool {
    true
}

pub fn layout() -> io::Result<Layout> {
    let json = |what: &str| -> io::Result<String> { request(&format!("j/{what}")) };
    let parse_err = |e: serde_json::Error| io::Error::new(io::ErrorKind::InvalidData, e);
    let monitors: Vec<LayoutMonitor> = serde_json::from_str(&json("monitors")?).map_err(parse_err)?;
    let mut workspaces: Vec<LayoutWorkspace> = serde_json::from_str(&json("workspaces")?).map_err(parse_err)?;
    let clients: Vec<LayoutClient> = serde_json::from_str(&json("clients")?).map_err(parse_err)?;
    workspaces.retain(|w| w.id > 0);
    workspaces.sort_by_key(|w| w.id);
    Ok(Layout {
        monitors: monitors
            .into_iter()
            .map(|m| {
                // Sizes come in pixels; a quarter turn swaps them.
                let (w, h) = (m.width / m.scale, m.height / m.scale);
                let (width, height) = if m.transform % 2 == 1 { (h, w) } else { (w, h) };
                Screen { name: m.name, x: m.x, y: m.y, width, height, active: m.active_workspace.id }
            })
            .collect(),
        workspaces: workspaces.into_iter().map(|w| Space { id: w.id, monitor: w.monitor }).collect(),
        windows: clients
            .into_iter()
            .filter(|c| c.mapped && c.workspace.id > 0)
            .map(|c| Placed {
                address: c.address,
                workspace: c.workspace.id,
                class: c.class,
                title: c.title,
                x: c.at[0],
                y: c.at[1],
                width: c.size[0],
                height: c.size[1],
                floating: c.floating,
            })
            .collect(),
    })
}

/// Ends the Hyprland session.
pub fn exit() {
    command("exiting", "dispatch hl.dsp.exit()");
}

thread_local! {
    /// How many surfaces are holding blur on, and the settings from before
    /// the first did.
    static BLUR_HOLDS: std::cell::RefCell<(u32, Option<(bool, i64)>)> = const { std::cell::RefCell::new((0, None)) };
}

/// Turns Hyprland's blur on at `size` for a surface that wants the desktop
/// blurred behind it, until a matching [`release_blur`]. The first hold saves
/// the user's settings and the last release puts them back.
pub fn hold_blur(size: i64) {
    BLUR_HOLDS.with(|h| {
        let mut h = h.borrow_mut();
        if h.0 == 0 {
            let enabled = option_json("decoration:blur:enabled")
                .map(|v| v.get("bool").and_then(|b| b.as_bool()).or_else(|| v.get("int").and_then(|i| i.as_i64()).map(|i| i != 0)).unwrap_or(false))
                .unwrap_or(false);
            let saved = option_json("decoration:blur:size").and_then(|v| v.get("int")?.as_i64()).filter(|&s| s > 0).unwrap_or(5);
            h.1 = Some((enabled, saved));
        }
        h.0 += 1;
    });
    set_blur(true, size);
}

pub fn release_blur() {
    BLUR_HOLDS.with(|h| {
        let mut h = h.borrow_mut();
        h.0 = h.0.saturating_sub(1);
        if h.0 == 0 {
            if let Some((enabled, size)) = h.1.take() {
                set_blur(enabled, size);
            }
        }
    });
}

fn set_blur(enabled: bool, size: i64) {
    eval(&format!("hl.config({{ decoration = {{ blur = {{ enabled = {enabled}, size = {} }} }} }})", size.max(1)));
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
