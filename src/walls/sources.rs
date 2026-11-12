//! Where wallpapers come from: wallhaven's API (images), MotionBGs and
//! MoeWalls (live, read off their listing pages, which have no API), and the
//! local library. Everything here blocks (curl, ffmpeg, disk), so it runs on
//! worker threads.
//!
//! The two live sites are scraped as Ryoku did; if a site changes its pages,
//! its search comes back empty or with an error rather than breaking others.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gtk::glib;
use gtk4 as gtk;

use crate::wallpaper;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";
const WALLHAVEN: &str = "https://wallhaven.cc/api/v1";
const MOTIONBGS: &str = "https://motionbgs.com";
const MOEWALLS: &str = "https://moewalls.com";
/// Wallhaven's aspect ratios, for "fit my screen".
const RATIOS: &[(u32, u32)] = &[(16, 9), (16, 10), (21, 9), (32, 9), (48, 9), (9, 16), (10, 16), (4, 3), (5, 4), (3, 2)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Wallhaven,
    MotionBgs,
    MoeWalls,
    Library,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Wallhaven, Source::MotionBgs, Source::MoeWalls, Source::Library];

    pub fn name(self) -> &'static str {
        match self {
            Source::Wallhaven => "Wallhaven",
            Source::MotionBgs => "MotionBGs",
            Source::MoeWalls => "MoeWalls",
            Source::Library => "Library",
        }
    }

    pub fn remote(self) -> bool {
        self != Source::Library
    }

    /// The Referer the site wants on its images and videos.
    fn referer(self) -> Option<&'static str> {
        match self {
            Source::MotionBgs => Some("https://motionbgs.com/"),
            Source::MoeWalls => Some("https://moewalls.com/"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    All,
    Images,
    Live,
}

#[derive(Clone)]
pub struct Query {
    pub text: String,
    pub page: u32,
    /// Wallhaven: the month's top list rather than the latest.
    pub top: bool,
    /// Wallhaven: only the screen's aspect ratio.
    pub fit: Option<(i32, i32)>,
    /// Library: which kind.
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub source: Source,
    /// A small picture: a URL, or a local file.
    pub thumb: String,
    /// What the preview shows: a big image URL, or a clip URL for live ones.
    pub preview: String,
    /// What a download fetches.
    pub download: String,
    /// The site's page for it (MoeWalls' master download hangs off it).
    pub page: Option<String>,
    pub resolution: String,
    pub live: bool,
    /// For the library: the file.
    pub local: Option<PathBuf>,
}

impl Item {
    pub fn referer(&self) -> Option<&'static str> {
        self.source.referer()
    }

    /// Where a download of it lands.
    pub fn target(&self) -> PathBuf {
        if let Some(local) = &self.local {
            return local.clone();
        }
        let ext = self.download.rsplit('.').next().filter(|e| e.len() <= 4 && !e.contains('/')).unwrap_or(if self.live { "mp4" } else { "jpg" });
        let ext = if self.source == Source::MotionBgs || (self.source == Source::MoeWalls && self.page.is_some()) { "mp4" } else { ext };
        let dir = if self.live { wallpaper::live_dir() } else { wallpaper::dir() };
        let prefix = match self.source {
            Source::Wallhaven => "wallhaven",
            Source::MotionBgs => "motionbgs",
            Source::MoeWalls => "moewalls",
            Source::Library => "",
        };
        dir.join(format!("{prefix}-{}.{ext}", self.id))
    }

    /// Already in the library.
    pub fn saved(&self) -> bool {
        self.local.is_some() || self.target().is_file()
    }
}

fn curl(url: &str, referer: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("curl");
    command.args(["-fsSL", "--max-time", "20", "-A", UA]);
    if let Some(r) = referer {
        command.args(["-e", r]);
    }
    if url.starts_with(WALLHAVEN) {
        if let Some(key) = wallhaven_key() {
            command.args(["-H", &format!("X-API-Key: {key}")]);
        }
    }
    let out = command.arg(url).stderr(Stdio::null()).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{} didn't answer", url.split('/').nth(2).unwrap_or(url)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// From WALLHAVEN_API_KEY or ~/.config/eclipse-shell/wallhaven-key; it only
/// raises wallhaven's rate limits here.
fn wallhaven_key() -> Option<String> {
    std::env::var("WALLHAVEN_API_KEY")
        .ok()
        .or_else(|| std::fs::read_to_string(glib::user_config_dir().join("eclipse-shell").join("wallhaven-key")).ok())
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn encode(text: &str) -> String {
    glib::Uri::escape_string(text, None, false).to_string()
}

pub fn search(source: Source, query: &Query) -> Result<Vec<Item>, String> {
    match source {
        Source::Wallhaven => wallhaven(query),
        Source::MotionBgs => motionbgs(query),
        Source::MoeWalls => moewalls(query),
        Source::Library => Ok(library(query)),
    }
}

fn wallhaven(query: &Query) -> Result<Vec<Item>, String> {
    let mut url = format!("{WALLHAVEN}/search?page={}&purity=100", query.page);
    url += if query.top { "&sorting=toplist&topRange=1M" } else { "&sorting=date_added" };
    if !query.text.is_empty() {
        url += &format!("&q={}", encode(&query.text));
    }
    if let Some((w, h)) = query.fit {
        let screen = f64::from(w) / f64::from(h.max(1));
        if let Some((rw, rh)) = RATIOS.iter().min_by(|a, b| (f64::from(a.0) / f64::from(a.1) - screen).abs().total_cmp(&(f64::from(b.0) / f64::from(b.1) - screen).abs())) {
            url += &format!("&ratios={rw}x{rh}");
        }
    }
    let json: serde_json::Value = serde_json::from_str(&curl(&url, None)?).map_err(|_| "wallhaven sent something unexpected".to_owned())?;
    let items = json["data"].as_array().cloned().unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|d| {
            let id = d["id"].as_str()?.to_owned();
            Some(Item {
                name: id.clone(),
                id,
                source: Source::Wallhaven,
                thumb: d["thumbs"]["small"].as_str()?.to_owned(),
                preview: d["path"].as_str()?.to_owned(),
                download: d["path"].as_str()?.to_owned(),
                page: d["url"].as_str().map(str::to_owned),
                resolution: d["resolution"].as_str().unwrap_or_default().to_owned(),
                live: false,
                local: None,
            })
        })
        .collect())
}

/// Every substring of `html` starting with `start` up to (not including) the
/// first of `stops`.
fn spans<'a>(html: &'a str, start: &str, stops: &[char]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(start) {
        let tail = &rest[at..];
        let end = tail.find(|c| stops.contains(&c)).unwrap_or(tail.len());
        out.push(&tail[..end]);
        rest = &tail[end.max(start.len())..];
    }
    out
}

/// The text between `before` and the next `after`, after the first `before`.
fn between<'a>(text: &'a str, before: &str, after: &str) -> Option<&'a str> {
    let start = text.find(before)? + before.len();
    let end = text[start..].find(after)? + start;
    Some(&text[start..end])
}

fn motionbgs(query: &Query) -> Result<Vec<Item>, String> {
    let path = if query.text.is_empty() {
        if query.page > 1 { format!("/{}/", query.page) } else { "/".to_owned() }
    } else {
        // Tags are lowercase with dashes; pages only as a trailing path.
        let tag = query.text.trim().to_lowercase().replace(' ', "-");
        let tag = encode(&tag);
        if query.page > 1 { format!("/tag:{tag}/{}/", query.page) } else { format!("/tag:{tag}/") }
    };
    let html = curl(&format!("{MOTIONBGS}{path}"), Some("https://motionbgs.com/"))?;
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    // Posters: /i/c/{W}x{H}/media/{id}/{name}[.{res}].jpg
    for poster in spans(&html, "/i/c/", &['"', '\'', ' ', ')']) {
        let parts: Vec<&str> = poster.split('/').collect();
        let [_, "i", "c", size, "media", id, file] = parts.as_slice() else { continue };
        if !(file.ends_with(".jpg") || file.ends_with(".jpeg")) || !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        // Skip icon-sized crops.
        if size.split('x').next().and_then(|w| w.parse::<u32>().ok()).is_none_or(|w| w < 300) || !seen.insert(id.to_string()) {
            continue;
        }
        let stem = file.trim_end_matches(".jpeg").trim_end_matches(".jpg");
        let (base, resolution) = match stem.rsplit_once('.') {
            Some((b, r)) if r.contains('x') && r.chars().all(|c| c.is_ascii_digit() || c == 'x') => (b, r.to_owned()),
            _ => (stem, String::new()),
        };
        items.push(Item {
            id: id.to_string(),
            name: title_case(&base.replace('-', " ")),
            source: Source::MotionBgs,
            thumb: format!("{MOTIONBGS}{poster}"),
            preview: format!("{MOTIONBGS}/dl/hd/{id}/"),
            download: format!("{MOTIONBGS}/dl/4k/{id}/"),
            page: None,
            resolution,
            live: true,
            local: None,
        });
        if items.len() >= 24 {
            break;
        }
    }
    Ok(items)
}

fn moewalls(query: &Query) -> Result<Vec<Item>, String> {
    let url = match (query.text.is_empty(), query.page > 1) {
        (false, false) => format!("{MOEWALLS}/?s={}", encode(&query.text)),
        (false, true) => format!("{MOEWALLS}/page/{}/?s={}", query.page, encode(&query.text)),
        (true, false) => format!("{MOEWALLS}/anime/"),
        (true, true) => format!("{MOEWALLS}/anime/page/{}/", query.page),
    };
    let html = curl(&url, Some("https://moewalls.com/"))?;
    let html = html.replace('\n', " ");
    let mut items = Vec::new();
    for card in html.split("<article ").skip(1).filter(|c| c.contains("g1-frame")) {
        // The poster: /uploads/YYYY/MM/BASE-thumb-WxH.jpg
        let Some(src) = spans(card, "src=\"https://moewalls.com/wp-content/uploads/", &['"']).into_iter().map(|s| &s[5..]).find(|s| s.contains("-thumb-")) else { continue };
        let rel = src.split("/uploads/").nth(1).unwrap_or_default();
        let year = rel.split('/').next().unwrap_or_default();
        let Some(base) = rel.rsplit('/').next().and_then(|f| f.split("-thumb-").next()) else { continue };
        let title = between(card, "<a title=\"", "\" class=\"g1-frame\"").map(|t| t.trim_end_matches(" Live Wallpaper").to_owned());
        let preview = format!("{MOEWALLS}/wp-content/uploads/preview/{year}/{base}-preview.webm");
        items.push(Item {
            id: base.to_owned(),
            name: title.map(|t| html_unescape(&t)).unwrap_or_else(|| title_case(&base.replace('-', " "))),
            source: Source::MoeWalls,
            thumb: src.to_owned(),
            preview: preview.clone(),
            download: preview,
            page: between(card, "class=\"g1-frame\" href=\"", "\"").map(str::to_owned),
            resolution: between(card, "resolutions-", " ").or_else(|| between(card, "resolutions-", "\"")).unwrap_or_default().to_owned(),
            live: true,
            local: None,
        });
    }
    Ok(items)
}

fn library(query: &Query) -> Vec<Item> {
    let needle = query.text.to_lowercase();
    wallpaper::list()
        .into_iter()
        .filter(|p| match query.kind {
            Kind::All => true,
            Kind::Images => !wallpaper::is_video(p),
            Kind::Live => wallpaper::is_video(p),
        })
        .filter_map(|p| {
            let name = p.file_stem()?.to_string_lossy().into_owned();
            if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                return None;
            }
            let live = wallpaper::is_video(&p);
            let file = p.display().to_string();
            Some(Item {
                id: name.clone(),
                name,
                source: Source::Library,
                thumb: file.clone(),
                preview: file.clone(),
                download: file,
                page: None,
                resolution: String::new(),
                live,
                local: Some(p),
            })
        })
        .collect()
}

fn title_case(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn html_unescape(text: &str) -> String {
    text.replace("&#8217;", "\u{2019}").replace("&#8211;", "\u{2013}").replace("&#038;", "&").replace("&amp;", "&").replace("&quot;", "\"").replace("&#039;", "'")
}

fn cache_dir(kind: &str) -> PathBuf {
    glib::user_cache_dir().join("eclipse-shell").join("walls").join(kind)
}

fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let ext = url.rsplit('.').next().filter(|e| e.len() <= 4 && !e.contains('/')).unwrap_or("img");
    format!("{:016x}.{ext}", hasher.finish())
}

/// A local copy of a remote picture (thumbnail or full image), cached.
pub fn fetch_picture(url: &str, referer: Option<&str>, kind: &str) -> Option<PathBuf> {
    if !url.starts_with("http") {
        let path = PathBuf::from(url);
        return if wallpaper::is_video(&path) { wallpaper::poster(&path) } else { Some(path) };
    }
    let dir = cache_dir(kind);
    let out = dir.join(cache_name(url));
    if out.is_file() {
        return Some(out);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let partial = out.with_extension("part");
    let mut command = Command::new("curl");
    command.args(["-fsSL", "--max-time", "60", "-A", UA]);
    if let Some(r) = referer {
        command.args(["-e", r]);
    }
    let ok = command.arg(url).arg("-o").arg(&partial).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if ok && std::fs::rename(&partial, &out).is_ok() {
        Some(out)
    } else {
        let _ = std::fs::remove_file(&partial);
        None
    }
}

/// Saves `item` into the library and returns where. `progress` gets the
/// bytes so far, read off the partial file.
pub fn download(item: &Item, progress: &std::sync::atomic::AtomicU64) -> Result<PathBuf, String> {
    let out = item.target();
    if out.is_file() {
        return Ok(out);
    }
    std::fs::create_dir_all(out.parent().ok_or("no folder")?).map_err(|e| e.to_string())?;
    // MoeWalls: the master hides behind a token on the post page; else the preview loop.
    let mut url = item.download.clone();
    let mut referer = item.referer().map(str::to_owned);
    if item.source == Source::MoeWalls {
        if let Some(post) = &item.page {
            if let Ok(page) = curl(post, None) {
                let token = spans(&page, "<a", &['>']).into_iter().find(|t| t.contains("id=\"moe-download\"")).and_then(|t| between(t, "data-url=\"", "\"").map(str::to_owned));
                if let Some(token) = token.filter(|t| t.chars().all(|c| c.is_ascii_alphanumeric() || "%+/=-_".contains(c))) {
                    url = format!("https://go.moewalls.com/download.php?video={token}");
                    referer = Some(post.clone());
                }
            }
        }
    }
    let partial = out.with_extension("part");
    let mut command = Command::new("curl");
    command.args(["-fsSL", "-A", UA]);
    if let Some(r) = &referer {
        command.args(["-e", r]);
    }
    let mut child = command.arg(&url).arg("-o").arg(&partial).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        progress.store(std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    if !status.success() || std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0) < 1024 {
        let _ = std::fs::remove_file(&partial);
        return Err("the download failed".into());
    }
    std::fs::rename(&partial, &out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Moves a library file to the trash.
pub fn trash(path: &Path) -> Result<(), String> {
    use gtk::gio::prelude::FileExt;
    gtk::gio::File::for_path(path).trash(gtk::gio::Cancellable::NONE).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_spans_and_betweens() {
        let html = r#"<img src="/i/c/546x308/media/1/a-b.3840x2160.jpg"> x /i/c/80x40/media/2/c.jpg'"#;
        assert_eq!(spans(html, "/i/c/", &['"', '\'']), vec!["/i/c/546x308/media/1/a-b.3840x2160.jpg", "/i/c/80x40/media/2/c.jpg"]);
        assert_eq!(between(r#"<a title="Hi there" class="g1-frame""#, "<a title=\"", "\" class"), Some("Hi there"));
    }

    #[test]
    fn title_cases() {
        assert_eq!(title_case("arlecchino girl behind"), "Arlecchino Girl Behind");
    }
}
