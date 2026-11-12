//! The wallpaper the desktop shows, for surfaces that draw it themselves (the
//! power menu's hero, the overview's workspaces).

use gtk::gio;
use gtk4 as gtk;

/// Decodes an image scaled to `height` device pixels tall, as cairo ARGB32
/// (premultiplied BGRA) rows. Runs on a worker thread.
pub fn decode_scaled(path: &str, height: i32) -> Option<(Vec<u8>, i32, i32)> {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, -1, height, true).ok()?;
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let (stride, channels) = (pixbuf.rowstride() as usize, pixbuf.n_channels() as usize);
    let bytes = pixbuf.read_pixel_bytes();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let row = &bytes[y * stride..];
        for x in 0..w as usize {
            let p = &row[x * channels..];
            let a = if channels == 4 { u32::from(p[3]) } else { 255 };
            let pre = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
            out.extend_from_slice(&[pre(p[2]), pre(p[1]), pre(p[0]), a as u8]);
        }
    }
    Some((out, w, h))
}

/// The image the wallpaper daemon (awww) is showing.
pub async fn current() -> Option<String> {
    let process = gio::Subprocess::newv(&["awww", "query"].map(std::ffi::OsStr::new), gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    // ": eDP-1: 1920x1200, scale: 1.5, currently displaying: image: /path"
    stdout?.lines().find_map(|l| l.split("image: ").nth(1)).map(|p| p.trim().to_owned())
}
