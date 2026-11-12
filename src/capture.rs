//! Window pictures for the overview, through Hyprland's toplevel-export
//! protocol (protocols/hyprland-toplevel-export-v1.xml). Hyprland draws a
//! window into a buffer of ours even while its workspace isn't showing.
//!
//! A thread of its own keeps a Wayland connection, takes batches of windows
//! to capture, and hands each picture back shrunk to the size asked for;
//! [`on_picture`] listeners get them on the main thread as textures. Nothing
//! runs between requests.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::sync::mpsc;

use gtk::{gdk, glib};
use gtk4 as gtk;
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};

#[allow(non_camel_case_types, non_upper_case_globals, unused_imports, clippy::all)]
mod export {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-export-v1.xml");
}

use export::hyprland_toplevel_export_frame_v1::{self as frame_v1, HyprlandToplevelExportFrameV1 as Frame};
use export::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1 as Manager;

/// A window to capture, by its Hyprland address, no wider than `max_width` px.
pub struct Job {
    pub address: String,
    pub max_width: u32,
}

/// A captured window, ready to become a texture.
struct Picture {
    address: String,
    width: u32,
    height: u32,
    /// B, G, R, A per pixel, rows packed.
    pixels: Vec<u8>,
}

thread_local! {
    static REQUESTS: RefCell<Option<mpsc::Sender<Vec<Job>>>> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn(&str, &gdk::Texture)>>> = RefCell::default();
}

/// Starts the capture thread, if Hyprland offers the protocol.
pub fn start() {
    let (requests, jobs) = mpsc::channel::<Vec<Job>>();
    let (pictures, arrived) = async_channel::unbounded::<Picture>();
    std::thread::spawn(move || {
        if let Err(err) = run(&jobs, &pictures) {
            eprintln!("eclipse-shell: window capture unavailable: {err}");
        }
    });
    REQUESTS.with(|r| *r.borrow_mut() = Some(requests));
    glib::spawn_future_local(async move {
        while let Ok(p) = arrived.recv().await {
            let bytes = glib::Bytes::from_owned(p.pixels);
            let texture: gdk::Texture = gdk::MemoryTexture::new(p.width as i32, p.height as i32, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, p.width as usize * 4).into();
            LISTENERS.with(|l| l.borrow().iter().for_each(|f| f(&p.address, &texture)));
        }
    });
}

/// Asks for these windows' pictures. A batch still waiting is replaced.
pub fn request(jobs: Vec<Job>) {
    REQUESTS.with(|r| {
        if let Some(r) = &*r.borrow() {
            let _ = r.send(jobs);
        }
    });
}

/// Calls `f` with each picture as it arrives.
pub fn on_picture(f: impl Fn(&str, &gdk::Texture) + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

// ---- the capture thread ------------------------------------------------------

#[derive(Default)]
struct State {
    shm: Option<wl_shm::WlShm>,
    manager: Option<Manager>,
    /// Frames under way, by the frame's protocol id.
    pending: HashMap<u32, Pending>,
    out: Vec<Picture>,
}

struct Pending {
    address: String,
    max_width: u32,
    /// The shm layout offered: format, width, height, stride.
    layout: Option<(wl_shm::Format, u32, u32, u32)>,
    buffer: Option<(File, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer)>,
    y_invert: bool,
    done: bool,
}

fn run(jobs: &mpsc::Receiver<Vec<Job>>, pictures: &async_channel::Sender<Picture>) -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::connect_to_env()?;
    let mut queue: EventQueue<State> = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state)?;
    let manager = state.manager.clone().ok_or("Hyprland doesn't offer toplevel export")?;

    while let Ok(mut batch) = jobs.recv() {
        // Only the latest batch matters.
        while let Ok(newer) = jobs.try_recv() {
            batch = newer;
        }
        for job in batch {
            let Some(handle) = u64::from_str_radix(job.address.trim_start_matches("0x"), 16).ok() else { continue };
            // The protocol takes the address's low 32 bits.
            let frame = manager.capture_toplevel(0, handle as u32, &qh, ());
            state.pending.insert(protocol_id(&frame), Pending { address: job.address, max_width: job.max_width, layout: None, buffer: None, y_invert: false, done: false });
        }
        while state.pending.values().any(|p| !p.done) {
            queue.blocking_dispatch(&mut state)?;
        }
        for (_, p) in state.pending.drain() {
            if let Some((_, pool, buffer)) = p.buffer {
                buffer.destroy();
                pool.destroy();
            }
        }
        for picture in state.out.drain(..) {
            let _ = pictures.send_blocking(picture);
        }
    }
    Ok(())
}

fn protocol_id(frame: &Frame) -> u32 {
    wayland_client::Proxy::id(frame).protocol_id()
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "hyprland_toplevel_export_manager_v1" => state.manager = Some(registry.bind(name, version.min(2), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<Frame, ()> for State {
    fn event(state: &mut Self, frame: &Frame, event: frame_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let id = protocol_id(frame);
        let Some(pending) = state.pending.get_mut(&id) else { return };
        match event {
            frame_v1::Event::Buffer { format: WEnum::Value(format @ (wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)), width, height, stride } => {
                pending.layout.get_or_insert((format, width, height, stride));
                // Version 1 has no buffer_done; copy as soon as there's a layout.
                if wayland_client::Proxy::version(frame) < 2 {
                    copy(state.shm.as_ref(), frame, pending, qh);
                }
            }
            frame_v1::Event::BufferDone => copy(state.shm.as_ref(), frame, pending, qh),
            frame_v1::Event::Flags { flags } => pending.y_invert = matches!(flags, WEnum::Value(f) if f.contains(frame_v1::Flags::YInvert)),
            frame_v1::Event::Ready { .. } => {
                if let Some(picture) = read(pending) {
                    state.out.push(picture);
                }
                pending.done = true;
                frame.destroy();
            }
            frame_v1::Event::Failed => {
                pending.done = true;
                frame.destroy();
            }
            _ => {}
        }
    }
}

/// Makes a buffer in the offered layout and asks for the frame in it.
fn copy(shm: Option<&wl_shm::WlShm>, frame: &Frame, pending: &mut Pending, qh: &QueueHandle<State>) {
    let (Some(shm), Some((format, width, height, stride)), None) = (shm, pending.layout, &pending.buffer) else {
        if pending.layout.is_none() || shm.is_none() {
            pending.done = true;
        }
        return;
    };
    let size = stride as usize * height as usize;
    // SAFETY: memfd_create returns a fresh descriptor (or -1) that the File
    // then owns and closes.
    let fd = unsafe { libc::memfd_create(c"eclipse-capture".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        pending.done = true;
        return;
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if file.set_len(size as u64).is_err() {
        pending.done = true;
        return;
    }
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());
    frame.copy(&buffer, 1);
    pending.buffer = Some((file, pool, buffer));
}

/// The captured pixels, upright, opaque where the format has no alpha, and
/// shrunk by whole steps to fit the width asked for.
fn read(pending: &Pending) -> Option<Picture> {
    let (format, width, height, stride) = pending.layout?;
    let (file, _, _) = pending.buffer.as_ref()?;
    let mut raw = vec![0u8; stride as usize * height as usize];
    file.read_exact_at(&mut raw, 0).ok()?;
    let step = width.div_ceil(pending.max_width.max(1)).max(1);
    let (w, h) = (width / step, height / step);
    if w == 0 || h == 0 {
        return None;
    }
    let opaque = format == wl_shm::Format::Xrgb8888;
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    let area = step * step;
    for y in 0..h {
        let target = if pending.y_invert { h - 1 - y } else { y };
        for x in 0..w {
            // Average the step×step block, channel by channel.
            let mut sum = [0u32; 4];
            for dy in 0..step {
                let row = (y * step + dy) as usize * stride as usize;
                for dx in 0..step {
                    let at = row + (x * step + dx) as usize * 4;
                    for c in 0..4 {
                        sum[c] += u32::from(raw[at + c]);
                    }
                }
            }
            let out = (target as usize * w as usize + x as usize) * 4;
            for c in 0..4 {
                pixels[out + c] = (sum[c] / area) as u8;
            }
            if opaque {
                pixels[out + 3] = 255;
            }
        }
    }
    Some(Picture { address: pending.address.clone(), width: w, height: h, pixels })
}

wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(State: ignore Manager);
