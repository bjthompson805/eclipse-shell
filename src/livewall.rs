//! The live wallpaper: `eclipse-shell livewall <clip>`, a process of its own
//! that loops a video on the background layer of one monitor.
//!
//! ffmpeg decodes in software, on one thread (VAAPI measured slower here,
//! the copy back costing more than it saves), at no more
//! than the monitor's logical width and 30 fps, and pipes raw frames here;
//! they go into shared-memory buffers, and the compositor scales them to cover
//! the screen through a viewport. No GPU context is ever opened, so the
//! process stays small. While a fullscreen window covers that monitor, or the
//! screen is off, ffmpeg is stopped (SIGSTOP) and frames stop.
//!
//! The shell starts and stops it (see [`crate::wallpaper`]); the still under
//! it, shown by awww, is the clip's first frame.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use wayland_client::protocol::{wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::hyprland;

pub const NAMESPACE: &str = "eclipse-shell-livewall";
const MAX_FPS: f64 = 30.0;
const MAX_WIDTH: u32 = 2560;
const BUFFERS: usize = 3;
/// How often to check whether the screen is covered.
const COVER_CHECK: Duration = Duration::from_secs(1);

struct Buffer {
    file: File,
    buffer: wl_buffer::WlBuffer,
    busy: bool,
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
    /// The size the compositor gave the surface, in logical px.
    configured: Option<(u32, u32)>,
    closed: bool,
    buffers: Vec<Buffer>,
}

/// Runs until the surface is closed or the decoder stops.
pub fn run(clip: &Path) -> Result<(), String> {
    let monitor = focused_monitor().ok_or("no monitor from Hyprland")?;
    let (video_w, video_h, video_fps) = probe(clip).ok_or("ffprobe could not read the clip")?;
    let width = video_w.min(monitor.logical_w).min(MAX_WIDTH).max(2) & !1;
    let height = ((f64::from(video_h) * f64::from(width) / f64::from(video_w)).round() as u32).max(2) & !1;
    let fps = video_fps.clamp(1.0, MAX_FPS);
    let frame_size = width as usize * height as usize * 4;

    // Frames come in on a thread.
    let mut decoder = Command::new("ffmpeg")
        // Paced to real time, with no burst after a pause.
        .args(["-nostdin", "-loglevel", "error", "-threads", "1", "-filter_threads", "1"])
        .args(["-readrate", "1", "-readrate_initial_burst", "0.5", "-readrate_catchup", "1", "-stream_loop", "-1", "-i"])
        .arg(clip)
        .args(["-an", "-vf", &format!("fps={fps},scale={width}:{height}:flags=bilinear"), "-pix_fmt", "bgr0", "-f", "rawvideo", "-"])
        // FreeBSD's jemalloc keeps freed 8 MB frames cached per thread and
        // arena; one arena, no thread cache and no decay holds ffmpeg near
        // 130 MB at 1080p instead of 400-800 MB and climbing, at no CPU cost.
        .env("MALLOC_CONF", "narenas:1,tcache:false,dirty_decay_ms:0,muzzy_decay_ms:0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start ffmpeg: {e}"))?;
    let mut pipe = decoder.stdout.take().ok_or("no ffmpeg output")?;
    // The reader keeps only the newest frame and wakes the loop through a
    // pipe, so ffmpeg never waits on us: when we fall behind, frames drop
    // rather than pile up in ffmpeg's queues.
    let latest: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let decoder_done = Arc::new(AtomicBool::new(false));
    let mut wake = [0i32; 2];
    // SAFETY: two ints for pipe() to fill.
    if unsafe { libc::pipe2(wake.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err("pipe failed".into());
    }
    {
        let (latest, done, wake_w) = (latest.clone(), decoder_done.clone(), wake[1]);
        std::thread::spawn(move || {
            loop {
                let mut frame = vec![0u8; frame_size];
                let ok = pipe.read_exact(&mut frame).is_ok();
                if ok {
                    *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
                } else {
                    done.store(true, Ordering::Relaxed);
                }
                // SAFETY: writing one byte from a live buffer; a full pipe
                // already holds a wake-up.
                unsafe { libc::write(wake_w, [1u8].as_ptr().cast(), 1) };
                if !ok {
                    break;
                }
            }
        });
    }

    let covered = Arc::new(AtomicBool::new(false));
    {
        let (covered, name) = (covered.clone(), monitor.name.clone());
        std::thread::spawn(move || {
            loop {
                covered.store(is_covered(&name), Ordering::Relaxed);
                std::thread::sleep(COVER_CHECK);
            }
        });
    }

    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State::default();
    // Globals, then the outputs' names.
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let (Some(compositor), Some(shm), Some(layer_shell), Some(viewporter)) = (state.compositor.clone(), state.shm.clone(), state.layer_shell.clone(), state.viewporter.clone()) else {
        return Err("the compositor lacks layer shell, shm or viewporter".into());
    };
    let output = state.outputs.iter().find(|(_, n)| n.as_deref() == Some(monitor.name.as_str())).map(|(o, _)| o.clone());

    let surface = compositor.create_surface(&qh, ());
    // Clicks go to the desktop beneath.
    let region = compositor.create_region(&qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();
    let layer = layer_shell.get_layer_surface(&surface, output.as_ref(), zwlr_layer_shell_v1::Layer::Background, NAMESPACE.into(), &qh, ());
    layer.set_anchor(zwlr_layer_surface_v1::Anchor::all());
    layer.set_exclusive_zone(-1);
    layer.set_size(0, 0);
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    let viewport = viewporter.get_viewport(&surface, &qh, ());
    surface.commit();
    while state.configured.is_none() && !state.closed {
        queue.blocking_dispatch(&mut state).map_err(|e| e.to_string())?;
    }
    let (screen_w, screen_h) = state.configured.unwrap_or((monitor.logical_w, monitor.logical_h));

    // Crop the frame to the screen's shape, then scale it to cover.
    let (fw, fh) = (f64::from(width), f64::from(height));
    let k = (f64::from(screen_w) / fw).max(f64::from(screen_h) / fh);
    let (crop_w, crop_h) = (f64::from(screen_w) / k, f64::from(screen_h) / k);
    viewport.set_source((fw - crop_w) / 2.0, (fh - crop_h) / 2.0, crop_w, crop_h);
    viewport.set_destination(screen_w as i32, screen_h as i32);

    for i in 0..BUFFERS {
        state.buffers.push(make_buffer(&shm, width, height, i, &qh)?);
    }

    let mut stopped = false;
    while !state.closed {
        let now_covered = covered.load(Ordering::Relaxed);
        if now_covered != stopped {
            stopped = now_covered;
            // SAFETY: signalling our own child by its pid.
            unsafe { libc::kill(decoder.id() as i32, if stopped { libc::SIGSTOP } else { libc::SIGCONT }) };
        }
        pump(&conn, &mut queue, &mut state, COVER_CHECK, wake[0])?;
        if decoder_done.load(Ordering::Relaxed) {
            return Err("the decoder stopped".into());
        }
        let Some(slot) = state.buffers.iter().position(|b| !b.busy) else { continue };
        let Some(frame) = latest.lock().unwrap_or_else(|e| e.into_inner()).take() else { continue };
        let buffer = &mut state.buffers[slot];
        if buffer.file.write_all_at(&frame, 0).is_err() {
            continue;
        }
        buffer.busy = true;
        surface.attach(Some(&buffer.buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();
        conn.flush().map_err(|e| e.to_string())?;
    }
    let _ = decoder.kill();
    Ok(())
}

/// Handles Wayland events, waiting up to `timeout` for one or for a wake-up
/// on `wake` (a new frame).
fn pump(conn: &Connection, queue: &mut EventQueue<State>, state: &mut State, timeout: Duration, wake: i32) -> Result<(), String> {
    queue.dispatch_pending(state).map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;
    if let Some(guard) = queue.prepare_read() {
        let mut fds = [libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 }];
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: two valid pollfds for the duration of the call.
        unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
        if fds[0].revents & libc::POLLIN != 0 {
            let _ = guard.read();
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut sink = [0u8; 64];
            // SAFETY: reading into a live buffer from our own non-blocking pipe.
            while unsafe { libc::read(wake, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        }
    }
    queue.dispatch_pending(state).map_err(|e| e.to_string())?;
    Ok(())
}

fn make_buffer(shm: &wl_shm::WlShm, width: u32, height: u32, index: usize, qh: &QueueHandle<State>) -> Result<Buffer, String> {
    let size = width as usize * height as usize * 4;
    // SAFETY: memfd_create returns a fresh descriptor (or -1) that the File
    // then owns and closes.
    let fd = unsafe { libc::memfd_create(c"eclipse-livewall".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err("memfd_create failed".into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64).map_err(|e| e.to_string())?;
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, width as i32 * 4, wl_shm::Format::Xrgb8888, qh, index);
    pool.destroy();
    Ok(Buffer { file, buffer, busy: false })
}

struct Monitor {
    name: String,
    logical_w: u32,
    logical_h: u32,
}

fn focused_monitor() -> Option<Monitor> {
    let monitors: serde_json::Value = serde_json::from_str(&hyprland::request("j/monitors").ok()?).ok()?;
    let list = monitors.as_array()?;
    let m = list.iter().find(|m| m["focused"].as_bool() == Some(true)).or(list.first())?;
    let scale = m["scale"].as_f64().unwrap_or(1.0).max(0.1);
    let logical = |key: &str| (m[key].as_f64().unwrap_or(1920.0) / scale).round() as u32;
    Some(Monitor { name: m["name"].as_str()?.to_owned(), logical_w: logical("width"), logical_h: logical("height") })
}

/// A fullscreen window on the monitor's workspace, or the screen is off.
fn is_covered(monitor: &str) -> bool {
    let read = |what: &str| hyprland::request(what).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let Some(monitors) = read("j/monitors") else { return false };
    let Some(m) = monitors.as_array().and_then(|l| l.iter().find(|m| m["name"].as_str() == Some(monitor)).cloned()) else { return false };
    if m["dpmsStatus"].as_bool() == Some(false) {
        return true;
    }
    let active = m["activeWorkspace"]["id"].as_i64();
    read("j/workspaces")
        .and_then(|w| w.as_array().cloned())
        .is_some_and(|list| list.iter().any(|w| w["id"].as_i64() == active && w["hasfullscreen"].as_bool() == Some(true)))
}

/// (width, height, frames per second) of the clip's video.
pub fn probe(clip: &Path) -> Option<(u32, u32, f64)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate", "-of", "csv=p=0:s=x"])
        .arg(clip)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split('x');
    let w = parts.next()?.parse().ok()?;
    let h = parts.next()?.parse().ok()?;
    let fps = parts.next().and_then(|r| {
        let (n, d) = r.split_once('/')?;
        Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?.max(1.0))
    });
    Some((w, h, fps.unwrap_or(30.0)))
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => state.layer_shell = Some(registry.bind(name, version.min(4), qh, ())),
                "wp_viewporter" => state.viewporter = Some(registry.bind(name, 1, qh, ())),
                "wl_output" if version >= 4 => state.outputs.push((registry.bind(name, 4, qh, ()), None)),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(state: &mut Self, output: &wl_output::WlOutput, event: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Name { name } = event {
            if let Some(entry) = state.outputs.iter_mut().find(|(o, _)| o == output) {
                entry.1 = Some(name);
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(state: &mut Self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                state.configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for State {
    fn event(state: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, index: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            if let Some(b) = state.buffers.get_mut(*index) {
                b.busy = false;
            }
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(wl_compositor::WlCompositor, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_region::WlRegion, zwlr_layer_shell_v1::ZwlrLayerShellV1, wp_viewporter::WpViewporter, wp_viewport::WpViewport);
