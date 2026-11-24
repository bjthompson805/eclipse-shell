//! eclipse-n1wifi: the root side of the N1 Wi-Fi device for eclipse-priv.
//! Installed root-owned at /usr/local/libexec/eclipse-n1wifi; eclipse-priv
//! checks its arguments first and this checks them again.
//!
//!   eclipse-n1wifi join <ssid-hex>    the PMK (64 hex digits) on stdin;
//!                                     waits for the outcome
//!   eclipse-n1wifi disconnect
//!   eclipse-n1wifi radio on|off
//!   eclipse-n1wifi ssid <ssid-hex>    print the SSID if loader.conf can
//!                                     quote it (printable ASCII, no " or \)
//!
//! The key goes on stdin, never in argv, where ps would show it.

// The bindings are shared with the shell, which uses the parts this doesn't.
#[allow(dead_code)]
#[path = "../n1wifi.rs"]
mod n1wifi;

use std::io::{self, Read};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use n1wifi::Device;

/// The driver scans first when the SSID isn't listed (up to ~10 s) and then
/// waits 15 s for the firmware.
const JOIN_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["join", ssid] => join(ssid),
        ["disconnect"] => open().and_then(|d| d.disconnect().map_err(|e| busy("disconnect", &e))),
        ["radio", state @ ("on" | "off")] => open().and_then(|d| d.radio(state == "on").map_err(|e| busy("radio", &e))),
        ["ssid", ssid] => loader_ssid(ssid),
        _ => Err("usage: eclipse-n1wifi join <ssid-hex> | disconnect | radio on|off | ssid <ssid-hex>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            ExitCode::FAILURE
        }
    }
}

fn open() -> Result<Device, String> {
    Device::open().map_err(|e| format!("{}: {e}", n1wifi::DEVICE))
}

fn busy(what: &str, e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EBUSY) => "Wi-Fi is busy joining a network; try again".into(),
        Some(libc::EPERM) => format!("{what}: needs root"),
        _ => format!("{what}: {e}"),
    }
}

fn ssid_arg(hex: &str) -> Result<Vec<u8>, String> {
    n1wifi::unhex(hex, 1, 32).ok_or_else(|| "the SSID must be 1 to 32 bytes in lower-case hex".into())
}

fn join(ssid_hex: &str) -> Result<(), String> {
    let ssid = ssid_arg(ssid_hex)?;
    let mut line = String::new();
    io::stdin().take(128).read_to_string(&mut line).map_err(|e| format!("stdin: {e}"))?;
    let pmk = n1wifi::unhex(line.trim_end_matches('\n'), 32, 32);
    line.clear();
    let Some(mut pmk) = pmk else { return Err("the key must be 64 lower-case hex digits on stdin".into()) };

    let device = open()?;
    let mut request = n1wifi::Join { ssid_len: ssid.len() as u8, ..n1wifi::Join::default() };
    request.ssid[..ssid.len()].copy_from_slice(&ssid);
    request.pmk.copy_from_slice(&pmk);
    pmk.fill(0);
    let before = device.status().map_err(|e| e.to_string())?.join_gen;
    let started = device.join(&mut request);
    request.pmk.fill(0);
    started.map_err(|e| match e.raw_os_error() {
        Some(libc::EBUSY) => "Wi-Fi is busy scanning or joining; try again".to_owned(),
        Some(libc::ENETDOWN) => "Wi-Fi is off".to_owned(),
        _ => format!("join: {e}"),
    })?;

    let deadline = Instant::now() + JOIN_WAIT;
    loop {
        let state = device.status().map_err(|e| e.to_string())?;
        if state.join_gen != before && state.flags & n1wifi::F_JOINING == 0 {
            return match state.join_error {
                0 => Ok(()),
                libc::ENOENT => Err("The network isn't in range".into()),
                libc::EOPNOTSUPP => Err("This network's security isn't supported".into()),
                libc::ETIMEDOUT => Err("Timed out joining; check the password".into()),
                libc::EIO => Err(format!("The network turned the connection down (status {}); check the password", state.join_status)),
                e => Err(format!("Joining failed: {}", io::Error::from_raw_os_error(e))),
            };
        }
        if Instant::now() >= deadline {
            return Err("Timed out joining the network".into());
        }
        sleep(POLL);
    }
}

fn loader_ssid(hex: &str) -> Result<(), String> {
    let ssid = ssid_arg(hex)?;
    if !ssid.iter().all(|&b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\') {
        return Err("this SSID can't be written to loader.conf (printable ASCII without \" or \\ only)".into());
    }
    println!("{}", String::from_utf8_lossy(&ssid));
    Ok(())
}
