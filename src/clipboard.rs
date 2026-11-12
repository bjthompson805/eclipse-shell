//! Clipboard history, kept by cliphist. The shell starts the wl-paste
//! watchers that record each copy (text and images) unless they're running
//! already, and reads, copies back, deletes and wipes through cliphist.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{gdk, gio, glib};
use gtk4 as gtk;

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// cliphist's id, newest highest.
    pub id: String,
    /// The text, or for an image cliphist's description of it.
    pub preview: String,
    pub image: Option<ImageInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageInfo {
    /// e.g. "82 KiB".
    pub size: String,
    /// e.g. "png".
    pub format: String,
    /// e.g. "810×888".
    pub dimensions: String,
}

thread_local! {
    /// Image entries' thumbnails, by id.
    static THUMBS: RefCell<HashMap<String, gdk::Texture>> = RefCell::default();
}

/// Starts recording copies, unless something already is.
pub fn start() {
    glib::spawn_future_local(async {
        for kind in ["text", "image"] {
            let pattern = format!("wl-paste --type {kind} --watch cliphist");
            if output(&["pgrep", "-f", &pattern]).await.is_some_and(|o| !o.trim().is_empty()) {
                continue;
            }
            let argv = ["wl-paste", "--type", kind, "--watch", "cliphist", "store"].map(std::ffi::OsStr::new);
            if let Err(e) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
                eprintln!("eclipse-shell: clipboard history isn't recording: {e}");
            }
        }
    });
}

/// The history, newest first.
pub async fn list() -> Vec<Clip> {
    output(&["cliphist", "list"]).await.map(|text| parse(&text)).unwrap_or_default()
}

fn parse(text: &str) -> Vec<Clip> {
    text.lines()
        .filter_map(|line| {
            let (id, preview) = line.split_once('\t')?;
            Some(Clip { id: id.to_owned(), preview: preview.to_owned(), image: image_info(preview) })
        })
        .collect()
}

/// "[[ binary data 82 KiB png 810x888 ]]" describes an image.
fn image_info(preview: &str) -> Option<ImageInfo> {
    let inner = preview.strip_prefix("[[ binary data ")?.strip_suffix(" ]]")?;
    let words: Vec<&str> = inner.split_whitespace().collect();
    let [number, unit, format, rest @ ..] = words.as_slice() else { return None };
    Some(ImageInfo { size: format!("{number} {unit}"), format: (*format).to_owned(), dimensions: rest.first().map(|d| d.replace('x', "\u{d7}")).unwrap_or_default() })
}

/// Puts an entry back on the clipboard.
pub fn copy(id: &str) {
    run_with_id("cliphist decode | wl-copy", id);
}

pub fn delete(id: &str) {
    run_with_id("cliphist delete", id);
    THUMBS.with(|t| t.borrow_mut().remove(id));
}

pub async fn wipe() {
    let _ = output(&["cliphist", "wipe"]).await;
    THUMBS.with(|t| t.borrow_mut().clear());
}

/// An image entry's picture, if it's been decoded.
pub fn cached_thumbnail(id: &str) -> Option<gdk::Texture> {
    THUMBS.with(|t| t.borrow().get(id).cloned())
}

/// An image entry's picture, decoded once and kept.
pub async fn thumbnail(id: &str) -> Option<gdk::Texture> {
    if let Some(known) = THUMBS.with(|t| t.borrow().get(id).cloned()) {
        return Some(known);
    }
    let process = gio::Subprocess::newv(
        &["sh", "-c", "printf '%s' \"$1\" | cliphist decode", "sh", id].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (bytes, _) = process.communicate_future(None).await.ok()?;
    let texture = gdk::Texture::from_bytes(&bytes?).ok()?;
    THUMBS.with(|t| t.borrow_mut().insert(id.to_owned(), texture.clone()));
    Some(texture)
}

/// Runs `script` with the entry's id on its standard input, as cliphist
/// takes it, without waiting.
fn run_with_id(script: &str, id: &str) {
    let script = format!("printf '%s' \"$1\" | {script}");
    let argv = ["sh", "-c", &script, "sh", id].map(std::ffi::OsStr::new);
    if let Ok(p) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        p.wait_async(gio::Cancellable::NONE, |_| {});
    }
}

async fn output(argv: &[&str]) -> Option<String> {
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE).ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    stdout.map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_and_images() {
        let clips = parse("2188\t[[ binary data 82 KiB png 810x888 ]]\n2187\tsudo make install\n");
        assert_eq!(clips.len(), 2);
        let image = clips[0].image.as_ref().expect("an image");
        assert_eq!((image.size.as_str(), image.format.as_str(), image.dimensions.as_str()), ("82 KiB", "png", "810\u{d7}888"));
        assert_eq!(clips[1].preview, "sudo make install");
        assert!(clips[1].image.is_none());
    }
}
