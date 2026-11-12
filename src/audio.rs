//! The PipeWire graph, as the mixer and the media popout see it: output and
//! input devices, the default of each, and the apps playing. The graph comes
//! from `pw-dump` and the volumes from `wpctl get-volume` (on OSS a node's own
//! Props don't carry the hardware volume). Changes go through `wpctl`.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk4 as gtk;

use crate::fader::{self, Fader};

/// The most often a drag sends a volume.
const SEND_EVERY: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Output,
    Input,
}

#[derive(Clone)]
pub struct Node {
    pub id: u32,
    pub label: String,
    pub glyph: &'static str,
    /// For an app: its icon name, if it gave one, and the class to look one up by.
    pub icon: Option<String>,
    pub app: String,
    /// For an app: its process, as PipeWire knows it.
    pub pid: Option<u32>,
    pub volume: f64,
    pub muted: bool,
}

#[derive(Default)]
pub struct Graph {
    pub outputs: Vec<Node>,
    pub inputs: Vec<Node>,
    pub streams: Vec<Node>,
    pub sink: Option<u32>,
    pub source: Option<u32>,
}

impl Graph {
    pub fn devices(&self, kind: Kind) -> &[Node] {
        match kind {
            Kind::Output => &self.outputs,
            Kind::Input => &self.inputs,
        }
    }

    pub fn current(&self, kind: Kind) -> Option<&Node> {
        let id = match kind {
            Kind::Output => self.sink,
            Kind::Input => self.source,
        }?;
        self.devices(kind).iter().find(|n| n.id == id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.outputs.iter().chain(&self.inputs).chain(&self.streams)
    }

    /// Everything the card's layout depends on, i.e. all but volumes and mutes.
    pub fn shape(&self, picking: Option<Kind>) -> String {
        let mut shape = format!("{:?} {:?} {}|", self.sink, self.source, picking.map_or(0, |k| k as u8 + 1));
        for node in self.nodes() {
            shape += &format!("{} {} {:?} {}|", node.id, node.label, node.icon, node.app);
        }
        shape
    }
}

pub async fn read() -> Graph {
    let mut graph = Graph::default();
    let Some(dump) = output(&["pw-dump"]).await else { return graph };
    let Ok(objects) = serde_json::from_str::<Vec<serde_json::Value>>(&dump) else { return graph };

    let mut defaults: HashMap<String, String> = HashMap::new();
    let mut ids: HashMap<String, u32> = HashMap::new();
    for object in &objects {
        let kind = object["type"].as_str().unwrap_or("");
        if kind.ends_with(":Metadata") {
            for entry in object["metadata"].as_array().into_iter().flatten() {
                if let (Some(key), Some(name)) = (entry["key"].as_str(), entry["value"]["name"].as_str()) {
                    defaults.insert(key.to_owned(), name.to_owned());
                }
            }
            continue;
        }
        if !kind.ends_with(":Node") {
            continue;
        }
        let Some(id) = object["id"].as_u64().and_then(|id| u32::try_from(id).ok()) else { continue };
        let props = &object["info"]["props"];
        let prop = |key: &str| props[key].as_str().filter(|v| !v.is_empty());
        let name = prop("node.name").unwrap_or("");
        ids.insert(name.to_owned(), id);
        let device_label = || prop("node.description").or(prop("node.nick")).unwrap_or(name).to_owned();
        let mut node = Node { id, label: String::new(), glyph: "speaker", icon: None, app: String::new(), pid: None, volume: 0.0, muted: false };
        match prop("media.class").unwrap_or("") {
            "Audio/Sink" => {
                node.label = device_label();
                node.glyph = device_glyph(Kind::Output, props);
                graph.outputs.push(node);
            }
            "Audio/Source" => {
                node.label = device_label();
                node.glyph = device_glyph(Kind::Input, props);
                graph.inputs.push(node);
            }
            "Stream/Output/Audio" => {
                node.label = prop("application.name").or(prop("media.name")).or(prop("node.description")).unwrap_or("Application").to_owned();
                node.icon = prop("application.icon-name").map(String::from);
                node.app = prop("application.process.binary").or(prop("application.name")).unwrap_or("").to_owned();
                node.pid = props["application.process.id"].as_str().and_then(|p| p.parse().ok()).or_else(|| props["application.process.id"].as_u64().and_then(|p| u32::try_from(p).ok()));
                graph.streams.push(node);
            }
            _ => {}
        }
    }
    graph.sink = defaults.get("default.audio.sink").and_then(|n| ids.get(n)).copied();
    graph.source = defaults.get("default.audio.source").and_then(|n| ids.get(n)).copied();

    // "48 Volume: 0.56 [MUTED]"
    let mut argv = vec!["sh".to_owned(), "-c".into(), r#"for i; do printf '%s ' "$i"; wpctl get-volume "$i"; done"#.into(), "sh".into()];
    argv.extend(graph.nodes().map(|n| n.id.to_string()));
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let volumes = output(&argv).await.unwrap_or_default();
    let volumes: HashMap<u32, (f64, bool)> = volumes
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let id = words.next()?.parse().ok()?;
            let volume = words.nth(1)?.parse().ok()?;
            Some((id, (volume, line.contains("MUTED"))))
        })
        .collect();
    for node in graph.outputs.iter_mut().chain(&mut graph.inputs).chain(&mut graph.streams) {
        if let Some(&(volume, muted)) = volumes.get(&node.id) {
            (node.volume, node.muted) = (volume, muted);
        }
    }
    graph
}

fn device_glyph(kind: Kind, props: &serde_json::Value) -> &'static str {
    let hint = ["device.api", "device.icon-name", "node.name", "node.description"].map(|k| props[k].as_str().unwrap_or("")).join(" ").to_lowercase();
    match () {
        _ if ["bluez", "eclipse-bt", "headphone", "headset"].iter().any(|h| hint.contains(h)) => "headphones",
        _ if ["hdmi", "displayport"].iter().any(|h| hint.contains(h)) => "tv",
        _ if kind == Kind::Input => "mic",
        _ => "speaker",
    }
}

pub async fn output(argv: &[&str]) -> Option<String> {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    stdout.map(String::from)
}

pub fn spawn(argv: &[&str]) {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    if let Ok(p) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        p.wait_async(gio::Cancellable::NONE, |_| {});
    }
}


/// A fader that sets `node`'s volume and mute.
pub fn volume_fader(s: f64, node: &Node, glyphs: (&'static str, &'static str), glyph_px: f64, inline: bool) -> Rc<Fader> {
    let id = node.id.to_string();
    let options = fader::Options { glyphs, glyph_px, inline, floor: 0.0, send_every: SEND_EVERY };
    let mute_id = id.clone();
    let on_mute: Box<dyn Fn(bool)> = Box::new(move |muted| spawn(&["wpctl", "set-mute", &mute_id, if muted { "1" } else { "0" }]));
    Fader::new(s, node.volume, node.muted, options, move |v| spawn(&["wpctl", "set-volume", &id, &format!("{v:.3}")]), Some(on_mute))
}
