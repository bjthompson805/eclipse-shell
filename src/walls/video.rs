//! Plays a clip (a local file or a URL) into cairo surfaces at preview size,
//! as the live wallpaper does: ffmpeg decodes to raw frames on a thread, and
//! the newest frame is handed to the main thread. GTK has no media backend
//! here, so this is how live wallpapers preview.

use std::cell::RefCell;
use std::io::Read;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::{cairo, glib};
use gtk4 as gtk;

const FPS: u32 = 24;
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";

#[derive(Default)]
pub struct Player {
    /// The playing clip's stop flag.
    stop: RefCell<Option<Arc<AtomicBool>>>,
}

impl Player {
    pub fn new() -> Rc<Self> {
        Rc::default()
    }

    /// Plays `source`, `width` px wide, looping; `on_frame` gets each frame.
    pub fn play(&self, source: &str, referer: Option<&str>, width: i32, on_frame: impl Fn(cairo::ImageSurface) + 'static) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        *self.stop.borrow_mut() = Some(stop.clone());
        let (source, referer) = (source.to_owned(), referer.map(str::to_owned));
        let width = (width.max(16) / 2) * 2;
        let (tx, rx) = async_channel::bounded::<(Vec<u8>, i32)>(1);

        std::thread::spawn(move || {
            let remote = source.starts_with("http");
            let headers = |command: &mut Command| {
                if remote {
                    command.args(["-user_agent", UA]);
                    if let Some(r) = &referer {
                        command.args(["-referer", r]);
                    }
                }
            };
            // The height comes from the clip's shape.
            let mut probe = Command::new("ffprobe");
            probe.args(["-v", "error"]);
            headers(&mut probe);
            let Ok(out) = probe.args(["-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x"]).arg(&source).output() else { return };
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let Some((vw, vh)) = text.trim().split_once('x').and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.trim_end_matches('x').parse::<f64>().ok()?))) else { return };
            let height = ((vh * f64::from(width) / vw.max(1.0)).round() as i32 / 2 * 2).max(2);

            let mut command = Command::new("ffmpeg");
            command.args(["-nostdin", "-loglevel", "error", "-threads", "1", "-readrate", "1", "-stream_loop", "-1"]);
            headers(&mut command);
            let Ok(mut child) = command
                .arg("-i")
                .arg(&source)
                .args(["-an", "-vf", &format!("fps={FPS},scale={width}:{height}:flags=bilinear"), "-pix_fmt", "bgr0", "-f", "rawvideo", "-"])
                .env("MALLOC_CONF", "narenas:1,tcache:false,dirty_decay_ms:0,muzzy_decay_ms:0")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            let Some(mut pipe) = child.stdout.take() else { return };
            let frame_size = width as usize * height as usize * 4;
            while !stop.load(Ordering::Relaxed) {
                let mut frame = vec![0u8; frame_size];
                // Frames the main thread hasn't taken yet are replaced.
                if pipe.read_exact(&mut frame).is_err() || tx.force_send((frame, height)).is_err() {
                    break;
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });

        let stopped = self.stop.borrow().clone();
        glib::spawn_future_local(async move {
            while let Ok((frame, height)) = rx.recv().await {
                if stopped.as_ref().is_some_and(|s| s.load(Ordering::Relaxed)) {
                    break;
                }
                if let Ok(surface) = cairo::ImageSurface::create_for_data(frame, cairo::Format::Rgb24, width, height, width * 4) {
                    on_frame(surface);
                }
            }
        });
    }

    pub fn stop(&self) {
        if let Some(stop) = self.stop.borrow_mut().take() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}
