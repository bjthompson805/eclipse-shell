//! The Apple N1 Wi-Fi control device, /dev/n1wifi (M5 Macs). The firmware
//! does 802.11 and the WPA2 handshake itself; the host sees a plain Ethernet
//! interface, n1w0, and drives Wi-Fi with a few ioctls on the device. These
//! mirror sys/arm64/apple/apple_n1_wifi.h in freebsd-src-m5, which is the
//! authority for the layouts and numbers.
//!
//! Status and the scan list are open to anyone and a scan to wheel and
//! operator; joining, disconnecting and the radio need root, so the shell asks
//! eclipse-priv, which runs the eclipse-n1wifi helper (src/bin/).

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

pub const DEVICE: &str = "/dev/n1wifi";
pub const IFACE: &str = "n1w0";
pub const VERSION: u32 = 1;
pub const MAXBSS: usize = 64;

// n1wifi_bss.security
pub const SEC_PRIVACY: u16 = 0x0001;
pub const SEC_RSN: u16 = 0x0002;
pub const SEC_JOINABLE: u16 = 0x8000;

// n1wifi_status.flags
pub const F_UP: u32 = 0x0001;
pub const F_RADIO: u32 = 0x0002;
pub const F_SCANNING: u32 = 0x0004;
pub const F_JOINING: u32 = 0x0008;
pub const F_CONNECTED: u32 = 0x0010;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Bss {
    pub bssid: [u8; 6],
    pub ssid_len: u8,
    pub channel: u8,
    pub ssid: [u8; 32],
    pub rssi: i8,
    pub band: u8,
    pub security: u16,
    pub rsn_caps: u16,
    pub reserved: u16,
    pub scan_gen: u32,
}

#[repr(C)]
pub struct List {
    pub count: u32,
    pub scan_gen: u32,
    pub bss: [Bss; MAXBSS],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct State {
    pub version: u32,
    pub flags: u32,
    pub scan_gen: u32,
    pub join_gen: u32,
    pub join_error: i32,
    pub join_status: u16,
    pub mac: [u8; 6],
    pub ssid: [u8; 32],
    pub ssid_len: u8,
    pub bssid: [u8; 6],
    pub channel: u8,
    pub band: u8,
    pub rssi: i8,
    pub reserved: u16,
}

#[repr(C)]
#[derive(Default)]
pub struct Join {
    pub ssid: [u8; 32],
    pub ssid_len: u8,
    pub flags: u8,
    pub bssid: [u8; 6],
    pub pmk: [u8; 32],
}

impl Bss {
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_len).min(32)]
    }
}

impl State {
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_len).min(32)]
    }
}

// FreeBSD's <sys/ioccom.h>: direction, 13 bits of size, group, number.
const IOC_VOID: u64 = 0x2000_0000;
const IOC_OUT: u64 = 0x4000_0000;
const IOC_IN: u64 = 0x8000_0000;

const fn ioc(dir: u64, num: u64, len: usize) -> u64 {
    dir | ((len as u64 & 0x1fff) << 16) | ((b'N' as u64) << 8) | num
}

pub const IOC_STATUS: u64 = ioc(IOC_OUT, 1, size_of::<State>());
pub const IOC_LIST: u64 = ioc(IOC_OUT, 2, size_of::<List>());
pub const IOC_SCAN: u64 = ioc(IOC_VOID, 3, 0);
pub const IOC_JOIN: u64 = ioc(IOC_IN, 4, size_of::<Join>());
pub const IOC_DISCONNECT: u64 = ioc(IOC_VOID, 5, 0);
pub const IOC_RADIO: u64 = ioc(IOC_IN, 6, size_of::<libc::c_int>());

pub fn present() -> bool {
    std::path::Path::new(DEVICE).exists()
}

/// The open device; every command is an ioctl on it.
pub struct Device(File);

impl Device {
    pub fn open() -> io::Result<Self> {
        File::open(DEVICE).map(Self)
    }

    /// One ioctl; `arg` is the command's structure, or null for none.
    fn ioctl(&self, request: u64, arg: *mut libc::c_void) -> io::Result<()> {
        // SAFETY: each request is paired with a pointer to the #[repr(C)]
        // structure of the size encoded in it (or null for _IO commands).
        if unsafe { libc::ioctl(self.0.as_raw_fd(), request as libc::c_ulong, arg) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn status(&self) -> io::Result<State> {
        let mut state = State::default();
        self.ioctl(IOC_STATUS, (&raw mut state).cast())?;
        if state.version != VERSION {
            return Err(io::Error::other(format!("n1wifi interface version {}, built for {VERSION}", state.version)));
        }
        Ok(state)
    }

    /// The networks of the last completed scan, one per BSSID.
    pub fn list(&self) -> io::Result<Vec<Bss>> {
        let mut list = Box::new(List { count: 0, scan_gen: 0, bss: [Bss::default(); MAXBSS] });
        self.ioctl(IOC_LIST, (&raw mut *list).cast())?;
        Ok(list.bss[..(list.count as usize).min(MAXBSS)].to_vec())
    }

    /// Starts a scan; the results show up in [`Device::list`] in 5 to 10 s.
    pub fn scan(&self) -> io::Result<()> {
        self.ioctl(IOC_SCAN, std::ptr::null_mut())
    }

    /// Starts a join (root only); the outcome lands in the status's join_gen
    /// and join_error.
    pub fn join(&self, join: &mut Join) -> io::Result<()> {
        self.ioctl(IOC_JOIN, (join as *mut Join).cast())
    }

    pub fn disconnect(&self) -> io::Result<()> {
        self.ioctl(IOC_DISCONNECT, std::ptr::null_mut())
    }

    pub fn radio(&self, on: bool) -> io::Result<()> {
        let mut on = libc::c_int::from(on);
        self.ioctl(IOC_RADIO, (&raw mut on).cast())
    }
}

/// Lower-case hex, the form SSIDs and keys travel in between the shell,
/// eclipse-priv and the helper.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Strict lower-case hex of `min..=max` bytes.
pub fn unhex(text: &str, min: usize, max: usize) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if digits.len() % 2 != 0 || !(min * 2..=max * 2).contains(&digits.len()) {
        return None;
    }
    if !digits.iter().all(|d| matches!(d, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    (0..digits.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_c_header() {
        // sizeof and the ioctl numbers as the C compiler sees apple_n1_wifi.h.
        assert_eq!(size_of::<Bss>(), 52);
        assert_eq!(size_of::<List>(), 3336);
        assert_eq!(size_of::<State>(), 72);
        assert_eq!(size_of::<Join>(), 72);
        assert_eq!(IOC_STATUS, 0x4048_4e01);
        assert_eq!(IOC_LIST, 0x4d08_4e02);
        assert_eq!(IOC_SCAN, 0x2000_4e03);
        assert_eq!(IOC_JOIN, 0x8048_4e04);
        assert_eq!(IOC_DISCONNECT, 0x2000_4e05);
        assert_eq!(IOC_RADIO, 0x8004_4e06);
    }

    #[test]
    fn parses_hex_strictly() {
        assert_eq!(unhex("486f6d65", 1, 32).unwrap(), b"Home");
        assert!(unhex("", 1, 32).is_none());
        assert!(unhex("486F", 1, 32).is_none());
        assert!(unhex("486", 1, 32).is_none());
        assert!(unhex(&"00".repeat(33), 1, 32).is_none());
        assert_eq!(hex(b"Home"), "486f6d65");
    }
}
