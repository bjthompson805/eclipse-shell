//! Claude Code rate-limit readout: a sparkle and the five-hour window's
//! usage, tinted with the accent past 80%.
//!
//! Reads the OAuth token from Claude Code's credentials file and asks
//! Anthropic's usage endpoint every two minutes (it rate-limits anything
//! faster). The token goes to curl on stdin so it never shows in `ps`.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use gtk4 as gtk;
use serde::Deserialize;

use crate::theme::{self, TextStyle, SUBTLE};

const POLL: Duration = Duration::from_secs(120);
const WARN_ABOVE: u32 = 80;
const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Deserialize)]
struct Usage {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    utilization: Option<f64>,
}

pub fn agent_usage(s: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (3.0 * s).round() as i32);
    let glyph = theme::filled_icon_glyph("auto_awesome", 15.0 * s, SUBTLE);
    let value = gtk::Label::new(None);
    row.append(&glyph);
    row.append(&value);

    // Keeps the last good reading through failed or rate-limited checks.
    let percent: Rc<Cell<Option<u32>>> = Rc::default();
    let paint = Rc::new({
        let percent = percent.clone();
        move || {
            let p = percent.get();
            let color = if p.is_some_and(|p| p > WARN_ABOVE) { theme::palette().accent_lit } else { SUBTLE };
            glyph.set_attributes(theme::filled_icon_glyph("auto_awesome", 15.0 * s, color).attributes().as_ref());
            value.set_text(&p.map_or("\u{2013}".into(), |p| format!("{p}%")));
            TextStyle::new(9.5 * s, pango::Weight::Medium, color).apply(&value);
        }
    });
    paint();
    theme::on_palette_change({
        let paint = paint.clone();
        move || paint()
    });

    let owner = row.downgrade();
    glib::spawn_future_local(async move {
        while owner.upgrade().is_some() {
            if let Some(p) = fetch().await {
                percent.set(Some(p));
                paint();
            }
            glib::timeout_future(POLL).await;
        }
    });
    row
}

fn credentials_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::home_dir().join(".claude"))
        .join(".credentials.json")
}

async fn fetch() -> Option<u32> {
    let raw = std::fs::read_to_string(credentials_path()).ok()?;
    let token = serde_json::from_str::<Credentials>(&raw).ok()?.oauth?.access_token;
    // A curl config file on stdin carries the headers.
    let config = format!(
        "header = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\n"
    );
    let process = gio::Subprocess::newv(
        &["curl", "-s", "-f", "--max-time", "8", "-K", "-", ENDPOINT].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (stdout, _) = process.communicate_utf8_future(Some(config)).await.ok()?;
    // -f makes curl fail on an error status, so a 429 body never parses as usage.
    if !process.is_successful() {
        return None;
    }
    let usage: Usage = serde_json::from_str(stdout.as_deref()?).ok()?;
    let u = usage.five_hour?.utilization?;
    let weekly = usage.seven_day.and_then(|w| w.utilization).unwrap_or(0.0);
    // The endpoint has reported both fractions and percentages; any value
    // of 1 or more means percentages.
    let percent = if u >= 1.0 || weekly >= 1.0 { u } else { u * 100.0 };
    Some(percent.round().clamp(0.0, 100.0) as u32)
}
