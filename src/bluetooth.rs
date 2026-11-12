//! Bluetooth on FreeBSD's own stack: hccontrol for the adapter, links and
//! scans, sdpcontrol to learn what a device offers, and eclipse-priv for
//! everything that writes root's config (pairing, virtual_oss audio, bthidd
//! input, power).
//!
//! A device is paired when /etc/bluetooth/hosts names it. Connecting starts
//! the service that carries it: a virtual_oss profile for audio, bthidd for
//! keyboards and mice. Both outlive a reboot, so a paired headset comes back
//! by itself when it's switched on; [`start`] keeps a PipeWire sink for each
//! connected headset's profile, since PipeWire only finds kernel pcm devices.
//!
//! Everything but [`start`] blocks; callers run it off the main thread.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const HELPER: &str = "/usr/local/libexec/eclipse-priv";
const HOSTS: &str = "/etc/bluetooth/hosts";
/// eclipse-priv's marker for an adapter that's there but switched off.
const OFF_MARKER: &str = "/var/run/eclipse-bt.off";
/// How long an unpaired device a scan found stays listed.
const SEEN_FOR: Duration = Duration::from_secs(120);
const SINK_PREFIX: &str = "eclipse-bt";
const SINK_TICK: Duration = Duration::from_secs(3);

#[derive(Clone, Default, PartialEq)]
pub struct Status {
    /// There's an adapter, on or off.
    pub present: bool,
    pub enabled: bool,
    /// Paired devices, then unpaired ones a recent scan found; connected first.
    pub devices: Vec<Device>,
    /// eclipse-priv is installed, so the controls can change things.
    pub can_change: bool,
}

#[derive(Clone, Default, PartialEq)]
pub struct Device {
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub kind: Kind,
    /// A Material Symbols glyph for it.
    pub glyph: &'static str,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Audio,
    Input,
    Phone,
    Computer,
    #[default]
    Other,
}

impl Kind {
    /// The major device class of a class of device.
    fn from_class(class: u32) -> Self {
        match (class >> 8) & 0x1f {
            1 => Kind::Computer,
            2 => Kind::Phone,
            4 => Kind::Audio,
            5 => Kind::Input,
            _ => Kind::Other,
        }
    }

    pub fn label(self) -> Option<&'static str> {
        match self {
            Kind::Audio => Some("audio"),
            Kind::Input => Some("input"),
            Kind::Phone => Some("phone"),
            Kind::Computer => Some("computer"),
            Kind::Other => None,
        }
    }
}

/// The glyph for a device, the minor class telling a keyboard from a mouse
/// and headphones from a speaker.
fn glyph(kind: Kind, class: u32) -> &'static str {
    let minor = (class >> 2) & 0x3f;
    match kind {
        Kind::Audio if matches!(minor, 5 | 7) => "speaker",
        Kind::Audio => "headphones",
        Kind::Input if minor & 0x30 == 0x20 => "mouse",
        Kind::Input if matches!(minor & 0x0f, 1 | 2) => "sports_esports",
        Kind::Input => "keyboard",
        Kind::Phone => "smartphone",
        Kind::Computer => "computer",
        Kind::Other => "bluetooth",
    }
}

/// What scans and service queries learned about a device, cached so a
/// paired device keeps its glyph while it's out of range.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Seen {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    class: u32,
    #[serde(default)]
    kind: Option<Kind>,
    /// When a scan last found it, in Unix seconds.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    seen: u64,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

pub fn read() -> Status {
    let mut status = Status { can_change: std::path::Path::new(HELPER).exists(), ..Status::default() };
    let node = node();
    status.enabled = node.is_some();
    status.present = status.enabled || std::path::Path::new(OFF_MARKER).exists();
    let Some(node) = node else { return status };
    let links = links(&node);
    let paired = paired();
    let mut seen = load_seen();
    // Profiles say what a paired device is even if nothing else does.
    let profiles = audio_profiles();
    for address in paired.keys() {
        if profiles.contains(&profile(address)) {
            seen.entry(address.clone()).or_default().kind.get_or_insert(Kind::Audio);
        }
    }
    status.devices = devices(&paired, &links, &seen, now());
    status
}

fn devices(paired: &BTreeMap<String, String>, links: &BTreeSet<String>, seen: &HashMap<String, Seen>, now: u64) -> Vec<Device> {
    let make = |address: &str, name: &str, is_paired: bool| {
        let s = seen.get(address).cloned().unwrap_or_default();
        let kind = s.kind.unwrap_or_else(|| Kind::from_class(s.class));
        let name = [name, &s.name].into_iter().find(|n| !n.is_empty()).unwrap_or(address).to_owned();
        Device { address: address.to_owned(), name, paired: is_paired, connected: links.contains(address), kind, glyph: glyph(kind, s.class) }
    };
    let mut devices: Vec<Device> = paired.iter().map(|(address, name)| make(address, name, true)).collect();
    for (address, s) in seen {
        if !paired.contains_key(address) && s.seen > 0 && now.saturating_sub(s.seen) < SEEN_FOR.as_secs() {
            devices.push(make(address, "", false));
        }
    }
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.paired.cmp(&a.paired)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    devices
}

/// A ten-second inquiry, then each find's name, which can take seconds apiece.
pub fn scan() -> Result<(), String> {
    let node = node().ok_or("Bluetooth is off")?;
    let found = inquiry(&output("hccontrol", &["-N", "-n", &node, "inquiry"]));
    let mut seen = load_seen();
    for (address, class) in &found {
        let entry = seen.entry(address.clone()).or_default();
        entry.seen = now();
        if *class != 0 {
            entry.class = *class;
        }
    }
    save_seen(&seen);
    for address in found.keys() {
        if let Some(name) = remote_name(&node, address) {
            seen.entry(address.clone()).or_default().name = name;
            save_seen(&seen);
        }
    }
    Ok(())
}

/// Remembers the device, then connects it: hcsecd answers the PIN request
/// that first connection raises.
pub fn pair(address: &str) -> Result<(), String> {
    let mut name = load_seen().get(address).map(|s| s.name.clone()).unwrap_or_default();
    if name.is_empty() {
        name = node().and_then(|n| remote_name(&n, address)).unwrap_or_default();
    }
    helper(&["bt", "pair", address, &name])?;
    connect(address)
}

pub fn connect(address: &str) -> Result<(), String> {
    let browse = Command::new("sdpcontrol").args(["-a", address, "browse"]).output().map_err(|e| e.to_string())?;
    if !browse.status.success() {
        return Err("The device didn't answer. Is it on and in pairing mode?".into());
    }
    // HID is service 0x1124, an A2DP sink 0x110b.
    let services = String::from_utf8_lossy(&browse.stdout);
    let kind = if services.contains("(0x1124)") {
        Kind::Input
    } else if services.contains("(0x110b)") {
        Kind::Audio
    } else {
        return Err("It offers neither audio nor input, the two kinds FreeBSD can carry.".into());
    };
    let mut seen = load_seen();
    seen.entry(address.to_owned()).or_default().kind = Some(kind);
    save_seen(&seen);
    helper(&["bt", if kind == Kind::Audio { "audio" } else { "input" }, "on", address])
}

pub fn disconnect(address: &str) -> Result<(), String> {
    let service = if audio_profiles().contains(&profile(address)) { "audio" } else { "input" };
    helper(&["bt", service, "off", address])
}

pub fn forget(address: &str) -> Result<(), String> {
    helper(&["bt", "forget", address])
}

pub fn set_power(on: bool) -> Result<(), String> {
    helper(&["bt", "power", if on { "on" } else { "off" }])
}

fn helper(args: &[&str]) -> Result<(), String> {
    let out = Command::new("sudo").arg("-n").arg(HELPER).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    Err(if why.is_empty() || why.contains("sudo") { "Run install.sh to change Bluetooth settings.".into() } else { why })
}

// ---- the stack ---------------------------------------------------------------

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// The first HCI node, e.g. "ubt0hci", while the adapter is on.
fn node() -> Option<String> {
    output("hccontrol", &["read_node_list"]).lines().filter_map(|l| l.split_whitespace().next()).find(|n| n.ends_with("hci")).map(String::from)
}

/// Addresses with an open ACL link.
fn links(node: &str) -> BTreeSet<String> {
    parse_links(&output("hccontrol", &["-N", "-n", node, "read_connection_list"]))
}

fn parse_links(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| {
            let fields: Vec<&str> = l.split_whitespace().collect();
            (fields.len() >= 9 && is_address(fields[0]) && fields[2] == "ACL" && fields.last() == Some(&"OPEN")).then(|| fields[0].to_lowercase())
        })
        .collect()
}

/// Paired devices from the hosts file: address to name (underscores stand
/// in for spaces there).
fn paired() -> BTreeMap<String, String> {
    parse_hosts(&std::fs::read_to_string(HOSTS).unwrap_or_default())
}

fn parse_hosts(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let mut fields = l.split('#').next()?.split_whitespace();
            let address = fields.next()?.to_lowercase();
            let name = fields.next()?;
            is_address(&address).then(|| (address, name.replace('_', " ")))
        })
        .collect()
}

/// Addresses and classes of device from `hccontrol -N inquiry`, whose class
/// lines read "Class: 24:04:04", most significant byte first.
fn inquiry(text: &str) -> BTreeMap<String, u32> {
    let mut found = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(address) = line.strip_prefix("BD_ADDR:") {
            let address = address.trim().to_lowercase();
            current = is_address(&address).then_some(address);
            if let Some(a) = &current {
                found.entry(a.clone()).or_insert(0);
            }
        } else if let (Some(class), Some(address)) = (line.strip_prefix("Class:"), &current) {
            let hex: String = class.trim().trim_start_matches("0x").chars().filter(|c| *c != ':').collect();
            if let Ok(class) = u32::from_str_radix(&hex, 16) {
                found.insert(address.clone(), class);
            }
        }
    }
    found
}

fn remote_name(node: &str, address: &str) -> Option<String> {
    output("hccontrol", &["-N", "-n", node, "remote_name_request", address])
        .lines()
        .find_map(|l| l.trim().strip_prefix("Name:").map(|n| n.trim().to_owned()))
        .filter(|n| !n.is_empty())
}

/// eclipse-priv's profile name: "bt" and the address's last six digits.
fn profile(address: &str) -> String {
    let digits: String = address.chars().filter(|c| *c != ':').collect();
    format!("bt{}", &digits[digits.len().saturating_sub(6)..])
}

fn audio_profiles() -> BTreeSet<String> {
    output("sysrc", &["-n", "virtual_oss_configs"]).split_whitespace().map(String::from).collect()
}

fn is_address(s: &str) -> bool {
    s.len() == 17 && s.split(':').count() == 6 && s.split(':').all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn seen_file() -> PathBuf {
    gtk4::glib::user_cache_dir().join("eclipse-shell").join("bt-seen.json")
}

fn load_seen() -> HashMap<String, Seen> {
    std::fs::read_to_string(seen_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_seen(seen: &HashMap<String, Seen>) {
    let path = seen_file();
    if let (Some(dir), Ok(json)) = (path.parent(), serde_json::to_string(seen)) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(path, json);
    }
}

// ---- PipeWire sinks ----------------------------------------------------------

/// Keeps a PipeWire sink for each connected headset's virtual_oss device, on
/// a thread of its own. A new one becomes the default output, as on other
/// desktops; when the headset goes, its sink goes and WirePlumber falls back
/// to the output before it. Costs one directory read a tick while no
/// profile exists.
pub fn start() {
    std::thread::spawn(|| {
        let mut wanted: BTreeMap<String, String> = BTreeMap::new();
        let mut first = true;
        loop {
            let devices = dsp_devices();
            if !devices.is_empty() || !wanted.is_empty() || first {
                let connected = node().map(|n| links(&n)).unwrap_or_default();
                let now: BTreeMap<String, String> = devices
                    .into_iter()
                    .filter(|(profile, _)| connected.iter().any(|a| &self::profile(a) == profile))
                    .map(|(profile, path)| (format!("{SINK_PREFIX}-{profile}"), path))
                    .collect();
                if now != wanted || first {
                    sync_sinks(&now);
                    wanted = now;
                }
                first = false;
            }
            std::thread::sleep(SINK_TICK);
        }
    });
}

/// virtual_oss devices: profile to device path.
fn dsp_devices() -> BTreeMap<String, String> {
    let Ok(entries) = std::fs::read_dir("/dev") else { return BTreeMap::new() };
    entries
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let profile = name.strip_prefix("dsp.")?;
            (profile.len() == 8 && profile.starts_with("bt")).then(|| (profile.to_owned(), format!("/dev/{name}")))
        })
        .collect()
}

fn sync_sinks(wanted: &BTreeMap<String, String>) {
    let have: BTreeSet<String> = output("pactl", &["list", "short", "sinks"])
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .filter(|n| n.starts_with(SINK_PREFIX))
        .map(String::from)
        .collect();
    let names = paired();
    for (sink, path) in wanted {
        if have.contains(sink) {
            continue;
        }
        let profile = sink.trim_start_matches(SINK_PREFIX).trim_start_matches('-');
        let description = names.iter().find(|(a, _)| self::profile(a) == profile).map_or("Bluetooth audio", |(_, n)| n.as_str());
        let quote = |s: &str| serde_json::to_string(s).unwrap_or_default();
        let props = format!(
            "{{ factory.name=freebsd-oss.sink node.name={} node.description={} media.class=Audio/Sink api.freebsd-oss.dsp-path={} object.linger=true }}",
            quote(sink),
            quote(description),
            quote(path)
        );
        if Command::new("pw-cli").args(["create-node", "adapter", &props]).output().is_ok_and(|o| o.status.success()) {
            // The node takes a moment to register.
            for _ in 0..10 {
                if Command::new("pactl").args(["set-default-sink", sink]).output().is_ok_and(|o| o.status.success()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    for sink in have.difference(&wanted.keys().cloned().collect()) {
        let _ = Command::new("pw-cli").args(["destroy", sink]).output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hosts_links_and_inquiries() {
        let hosts = parse_hosts("# paired\n44:5E:CD:73:95:06\tBlackShark_V3_Pro bt739506\nnot-an-address x\n");
        assert_eq!(hosts.get("44:5e:cd:73:95:06").map(String::as_str), Some("BlackShark V3 Pro"));
        assert_eq!(hosts.len(), 1);

        let links = parse_links(
            "Remote BD_ADDR    Handle Type Mode Role Encrypt Pending Queue State\n\
             44:5e:cd:73:95:06   3860  ACL    0 MAST    NONE       0     0 OPEN\n\
             aa:bb:cc:dd:ee:ff   3861  ACL    0 MAST    NONE       0     0 W4_CONN_COMPLETE\n",
        );
        assert_eq!(links.into_iter().collect::<Vec<_>>(), ["44:5e:cd:73:95:06"]);

        let found = inquiry("Inquiry result, num_responses=1\nInquiry result #0\n\tBD_ADDR: 44:5e:cd:73:95:06\n\tClass: 24:04:04\n");
        assert_eq!(found.get("44:5e:cd:73:95:06"), Some(&0x240404));
        assert_eq!(Kind::from_class(0x240404), Kind::Audio);
        assert_eq!(glyph(Kind::Audio, 0x240404), "headphones");
        assert_eq!(profile("44:5e:cd:73:95:06"), "bt739506");
    }

    #[test]
    fn lists_connected_then_paired_then_found() {
        let paired = BTreeMap::from([("aa:aa:aa:aa:aa:01".to_owned(), "Zed".to_owned()), ("aa:aa:aa:aa:aa:02".to_owned(), "Amp".to_owned())]);
        let links = BTreeSet::from(["aa:aa:aa:aa:aa:01".to_owned()]);
        let seen = HashMap::from([
            ("aa:aa:aa:aa:aa:03".to_owned(), Seen { name: "Fresh".into(), seen: 1000, ..Seen::default() }),
            ("aa:aa:aa:aa:aa:04".to_owned(), Seen { name: "Stale".into(), seen: 10, ..Seen::default() }),
        ]);
        let names: Vec<String> = devices(&paired, &links, &seen, 1050).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Zed", "Amp", "Fresh"]);
    }
}
