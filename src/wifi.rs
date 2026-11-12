//! Wi-Fi and the network links, on FreeBSD's wpa_supplicant. Requests go to
//! the supplicant's control socket directly, as wpa_cli does, so a password
//! never sits in a process's argument list. The socket is reachable by wheel
//! (ctrl_interface_group in /etc/wpa_supplicant.conf).
//!
//! Everything here blocks; callers run it off the main thread.

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const CONTROL_DIR: &str = "/var/run/wpa_supplicant";
const REPLY_WAIT: Duration = Duration::from_secs(5);
const JOIN_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Default, PartialEq)]
pub struct Status {
    /// The Wi-Fi interface, if there is one.
    pub iface: Option<String>,
    pub radio: bool,
    /// The default route's interface.
    pub route: String,
    /// The wired link carrying the default route: its speed in Mb/s, if known.
    pub wired: Option<Option<u32>>,
    pub wired_ip: Option<String>,
    pub wifi_ip: Option<String>,
    /// One per SSID at its strongest access point, strongest first.
    pub networks: Vec<Network>,
    /// The supplicant answered.
    pub reachable: bool,
}

impl Status {
    pub fn joined(&self) -> Option<&Network> {
        self.networks.iter().find(|n| n.connected)
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct Network {
    pub ssid: String,
    /// 0 to 1.
    pub signal: f64,
    pub secured: bool,
    /// The supplicant has it configured.
    pub known: bool,
    pub connected: bool,
}

/// A Material Symbols glyph for a signal of 0 to 1.
pub fn signal_glyph(level: f64) -> &'static str {
    match level {
        l if l > 0.8 => "signal_wifi_4_bar",
        l if l > 0.55 => "network_wifi_3_bar",
        l if l > 0.3 => "network_wifi_2_bar",
        _ => "network_wifi_1_bar",
    }
}

pub fn read() -> Status {
    let route = default_route();
    let mut status = Status { route: route.clone(), ..Status::default() };
    if !route.is_empty() && !route.starts_with("wlan") {
        let ifconfig = output("ifconfig", &[&route]);
        status.wired = Some(media_speed(&ifconfig));
        status.wired_ip = inet(&ifconfig);
    }
    let Some(iface) = wlan_iface() else { return status };
    let ifconfig = output("ifconfig", &[&iface]);
    status.radio = ifconfig.lines().next().is_some_and(|l| l.contains("<UP"));
    status.wifi_ip = inet(&ifconfig);
    status.iface = Some(iface.clone());
    let Ok(control) = Control::open(&iface) else { return status };
    let (Ok(state), Ok(scan), Ok(list)) = (control.request("STATUS"), control.request("SCAN_RESULTS"), control.request("LIST_NETWORKS")) else {
        return status;
    };
    status.reachable = true;
    status.networks = networks(&state, &scan, &list);
    // Some drivers put the noise floor in the scan's signal column; the
    // station list has the joined network's real signal over noise.
    if let (Some(joined), Some(snr)) = (status.networks.iter_mut().find(|n| n.connected), station_snr(&iface)) {
        joined.signal = (snr / 40.0).clamp(0.0, 1.0);
    }
    status
}

/// Asks for a fresh scan; the results show up in a later [`read`].
pub fn scan() -> Result<(), String> {
    Control::open(&wlan_iface().ok_or("no Wi-Fi interface")?)?.ok("SCAN")
}

pub fn disconnect() -> Result<(), String> {
    Control::open(&wlan_iface().ok_or("no Wi-Fi interface")?)?.ok("DISCONNECT")
}

/// Joins `ssid`, adding it unless it's configured, and waits until it's up.
/// A new network that fails to join is removed again, so a wrong password
/// doesn't leave it looking known. SELECT_NETWORK disables every other
/// network, so they're enabled again either way before the config is saved.
pub fn connect(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let iface = wlan_iface().ok_or("no Wi-Fi interface")?;
    let control = Control::open(&iface)?;
    let events = Control::open(&iface)?;
    events.ok("ATTACH")?;

    let configured = configured(&control.request("LIST_NETWORKS")?);
    let (id, known) = match configured.into_iter().find(|(name, _)| name == ssid) {
        Some((_, id)) => (id, true),
        None => (add_network(&control, ssid, password.is_none())?, false),
    };
    let forget = |e: String| {
        if !known {
            let _ = control.ok(&format!("REMOVE_NETWORK {id}"));
        }
        e
    };
    if let Some(password) = password {
        let psk = psk(password).map_err(forget)?;
        control.ok(&format!("SET_NETWORK {id} psk {psk}")).map_err(forget)?;
    }
    control.ok(&format!("SELECT_NETWORK {id}")).map_err(forget)?;

    let joined = events.await_join(JOIN_WAIT);
    if joined.is_err() && !known {
        let _ = control.ok(&format!("REMOVE_NETWORK {id}"));
    }
    let _ = control.ok("ENABLE_NETWORK all");
    let _ = control.ok("SAVE_CONFIG");
    joined
}

fn add_network(control: &Control, ssid: &str, open: bool) -> Result<String, String> {
    let id = control.request("ADD_NETWORK")?.trim().to_owned();
    if id.parse::<u32>().is_err() {
        return Err(format!("ADD_NETWORK: {id}"));
    }
    let hex: String = ssid.bytes().map(|b| format!("{b:02x}")).collect();
    let mut set = control.ok(&format!("SET_NETWORK {id} ssid {hex}"));
    if set.is_ok() && open {
        set = control.ok(&format!("SET_NETWORK {id} key_mgmt NONE"));
    }
    if let Err(e) = set {
        let _ = control.ok(&format!("REMOVE_NETWORK {id}"));
        return Err(e);
    }
    Ok(id)
}

/// A password as SET_NETWORK psk takes it: a quoted passphrase of 8 to 63
/// printable ASCII characters, or a raw 64-digit hex key.
fn psk(password: &str) -> Result<String, String> {
    if password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(password.to_owned());
    }
    if !(8..=63).contains(&password.len()) {
        return Err("A Wi-Fi password is 8 to 63 characters".into());
    }
    if !password.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err("A Wi-Fi password is plain ASCII".into());
    }
    Ok(format!("\"{password}\""))
}

// ---- the control socket ------------------------------------------------------

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// One datagram socket to the supplicant, bound to a path of our own so it
/// can answer.
struct Control {
    socket: UnixDatagram,
    local: PathBuf,
}

impl Control {
    fn open(iface: &str) -> Result<Self, String> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let local = dir.join(format!("eclipse-wpa-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_file(&local);
        let unreachable = |e: std::io::Error| format!("wpa_supplicant isn't reachable on {iface}: {e}");
        let socket = UnixDatagram::bind(&local).map_err(unreachable)?;
        let control = Self { socket, local };
        control.socket.connect(PathBuf::from(CONTROL_DIR).join(iface)).map_err(unreachable)?;
        Ok(control)
    }

    /// Sends one request and returns its reply, skipping any event ("<N>...")
    /// that arrives in between on an attached socket.
    fn request(&self, request: &str) -> Result<String, String> {
        let failed = |e: std::io::Error| format!("{request}: {e}");
        self.socket.set_read_timeout(Some(REPLY_WAIT)).map_err(failed)?;
        self.socket.send(request.as_bytes()).map_err(failed)?;
        let mut buffer = vec![0; 65536];
        loop {
            let n = self.socket.recv(&mut buffer).map_err(failed)?;
            if buffer.first() != Some(&b'<') {
                return Ok(String::from_utf8_lossy(&buffer[..n]).into_owned());
            }
        }
    }

    /// Sends a request whose only good answer is OK.
    fn ok(&self, request: &str) -> Result<(), String> {
        let reply = self.request(request)?;
        if reply.trim() == "OK" {
            return Ok(());
        }
        let verb = request.split_whitespace().next().unwrap_or(request);
        Err(format!("{verb}: {}", reply.trim()))
    }

    /// Reads events until the link is up or an attempt is turned down.
    fn await_join(&self, within: Duration) -> Result<(), String> {
        let deadline = Instant::now() + within;
        let mut buffer = vec![0; 4096];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("Timed out joining the network".into());
            }
            let _ = self.socket.set_read_timeout(Some(left));
            let Ok(n) = self.socket.recv(&mut buffer) else { return Err("Timed out joining the network".into()) };
            let event = String::from_utf8_lossy(&buffer[..n]);
            if event.contains("CTRL-EVENT-CONNECTED") {
                return Ok(());
            }
            if event.contains("WRONG_KEY") {
                return Err("Wrong password".into());
            }
            if ["CTRL-EVENT-SSID-TEMP-DISABLED", "CTRL-EVENT-ASSOC-REJECT", "CTRL-EVENT-AUTH-REJECT"].iter().any(|e| event.contains(e)) {
                return Err("The network turned the connection down".into());
            }
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.local);
    }
}

// ---- parsing -----------------------------------------------------------------

/// Folds STATUS, SCAN_RESULTS and LIST_NETWORKS into one row per SSID,
/// strongest first, hidden networks left out.
fn networks(state: &str, scan: &str, list: &str) -> Vec<Network> {
    let field = |key: &str| state.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).unwrap_or("");
    let joined = (field("wpa_state") == "COMPLETED").then(|| unescape(field("ssid")));
    let known: Vec<String> = configured(list).into_iter().map(|(ssid, _)| ssid).collect();

    // Rows are "bssid / frequency / signal level / flags / ssid", tab separated.
    let mut networks: Vec<Network> = Vec::new();
    for row in scan.lines().skip(1) {
        let fields: Vec<&str> = row.splitn(5, '\t').collect();
        let [_, _, level, flags, ssid] = fields[..] else { continue };
        let ssid = unescape(ssid);
        if ssid.is_empty() {
            continue;
        }
        let signal = signal(level.parse().unwrap_or(0));
        if let Some(existing) = networks.iter_mut().find(|n| n.ssid == ssid) {
            existing.signal = existing.signal.max(signal);
            continue;
        }
        networks.push(Network {
            known: known.contains(&ssid),
            connected: joined.as_deref() == Some(ssid.as_str()),
            secured: ["WPA", "RSN", "WEP", "SAE", "EAP"].iter().any(|k| flags.contains(k)),
            signal,
            ssid,
        });
    }
    networks.sort_by(|a, b| b.signal.total_cmp(&a.signal));
    networks
}

/// Configured networks from LIST_NETWORKS rows "id / ssid / bssid / flags".
fn configured(list: &str) -> Vec<(String, String)> {
    list.lines().skip(1).filter_map(|row| {
        let mut fields = row.split('\t');
        let id = fields.next()?.to_owned();
        Some((unescape(fields.next()?), id))
    })
    .collect()
}

/// A scan level as 0 to 1. A negative level is dBm (-90 to -30); FreeBSD's
/// drivers report the signal over the noise floor in half dB, where 80 (40 dB)
/// is excellent.
fn signal(level: i32) -> f64 {
    let fraction = if level < 0 { f64::from(level + 90) / 60.0 } else { f64::from(level) / 80.0 };
    fraction.clamp(0.0, 1.0)
}

/// Undoes the supplicant's escaping of SSIDs (\n, \t, \e, \xNN, \\, \").
fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'e' => out.push(0x1b),
            b'x' if i + 2 < bytes.len() && u8::from_str_radix(&s[i + 1..i + 3], 16).is_ok() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 2;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- interfaces --------------------------------------------------------------

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn wlan_iface() -> Option<String> {
    output("ifconfig", &["-g", "wlan"]).split_whitespace().next().map(String::from)
}

fn default_route() -> String {
    output("route", &["-n", "get", "default"]).lines().find_map(|l| l.trim().strip_prefix("interface:")).unwrap_or("").trim().to_owned()
}

/// The joined access point's signal over noise in dB, from the RSSI column of
/// `ifconfig <iface> list sta`, where 40 dB and up is as good as it gets.
fn station_snr(iface: &str) -> Option<f64> {
    let table = output("ifconfig", &[iface, "list", "sta"]);
    let mut lines = table.lines();
    let column = lines.next()?.split_whitespace().position(|h| h == "RSSI")?;
    lines.next()?.split_whitespace().nth(column)?.parse().ok()
}

fn inet(ifconfig: &str) -> Option<String> {
    ifconfig.lines().find_map(|l| l.trim().strip_prefix("inet ")?.split_whitespace().next().map(String::from))
}

/// The link speed in Mb/s from ifconfig's media line, e.g. "1000baseT".
fn media_speed(ifconfig: &str) -> Option<u32> {
    let media = ifconfig.lines().find_map(|l| l.trim().strip_prefix("media:"))?;
    let media = media.to_lowercase();
    let word = media.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| w.contains("base"))?;
    let digits = word.split("base").next()?;
    let (number, giga) = match digits.strip_suffix('g') {
        Some(n) => (n, true),
        None => (digits, false),
    };
    let speed: u32 = number.parse().ok()?;
    Some(if giga { speed * 1000 } else { speed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_scan_rows_by_ssid() {
        let state = "bssid=aa:bb:cc:dd:ee:01\nssid=Home\nwpa_state=COMPLETED\n";
        let scan = "bssid / frequency / signal level / flags / ssid\n\
                    aa:bb:cc:dd:ee:01\t5180\t40\t[WPA2-PSK-CCMP][ESS]\tHome\n\
                    aa:bb:cc:dd:ee:02\t2412\t60\t[WPA2-PSK-CCMP][ESS]\tHome\n\
                    aa:bb:cc:dd:ee:03\t2437\t70\t[ESS]\tCafe\\x20Wi-Fi\n\
                    aa:bb:cc:dd:ee:04\t2462\t75\t[WPA2-PSK-CCMP][ESS]\t\n";
        let list = "network id / ssid / bssid / flags\n0\tHome\tany\t[CURRENT]\n";
        let nets = networks(state, scan, list);
        let names: Vec<&str> = nets.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Cafe Wi-Fi", "Home"]);
        let (cafe, home) = (&nets[0], &nets[1]);
        assert!(home.connected && home.known && home.secured);
        assert_eq!(home.signal, 0.75);
        assert!(!cafe.secured && !cafe.known && !cafe.connected);
    }

    #[test]
    fn checks_passwords() {
        assert_eq!(psk("hunter22").unwrap(), "\"hunter22\"");
        assert!(psk("short").is_err());
        assert_eq!(psk(&"a".repeat(64)).unwrap(), "a".repeat(64));
        assert!(psk("caf\u{e9}caf\u{e9}").is_err());
    }

    #[test]
    fn reads_media_speed() {
        assert_eq!(media_speed("\tmedia: Ethernet autoselect (1000baseT <full-duplex>)"), Some(1000));
        assert_eq!(media_speed("\tmedia: Ethernet autoselect (2500Base-T <full-duplex>)"), Some(2500));
        assert_eq!(media_speed("\tmedia: Ethernet 10GbaseT"), Some(10000));
    }
}
