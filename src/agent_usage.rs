//! Claude Code usage: the rate-limit windows from Anthropic's usage
//! endpoint, and token counts from Claude Code's local transcripts.
//!
//! The limits come from the OAuth token in Claude Code's credentials file,
//! checked every two minutes (the endpoint rate-limits anything faster). The
//! token goes to curl on stdin so it never shows in `ps`. A failed check
//! keeps the last good reading. Token counts are summed from the last eight
//! days of transcripts on a worker thread, per day and per model, counting
//! each message once.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gtk::{gio, glib};
use gtk4 as gtk;
use serde::Deserialize;

const POLL: Duration = Duration::from_secs(120);
const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const SCAN_DAYS: u64 = 8;

#[derive(Clone, Copy, Default)]
pub struct Limit {
    pub percent: u32,
    /// Unix seconds, when known.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Default)]
pub struct Usage {
    pub session: Option<Limit>,
    pub weekly: Option<Limit>,
    /// "Pro", "Max 20X", ...
    pub plan: String,
    /// Unix seconds of the last good reading, and of the last attempt.
    pub updated_at: Option<i64>,
    pub checked_at: Option<i64>,
    /// Why the last check gave nothing, if it didn't.
    pub problem: Option<String>,
    pub refreshing: bool,
    /// Tokens per local day ("2026-11-11"), oldest first.
    pub days: Vec<(String, u64)>,
    /// Tokens per model, largest first.
    pub models: Vec<(String, u64)>,
}

thread_local! {
    static USAGE: RefCell<Usage> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
    static WAKE: RefCell<Option<async_channel::Sender<()>>> = RefCell::default();
}

pub fn current() -> Usage {
    USAGE.with(|u| u.borrow().clone())
}

pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

fn update(f: impl FnOnce(&mut Usage)) {
    USAGE.with(|u| f(&mut u.borrow_mut()));
    LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
}

/// Checks again now rather than at the next tick.
pub fn refresh_now() {
    WAKE.with(|w| {
        if let Some(wake) = w.borrow().as_ref() {
            let _ = wake.try_send(());
        }
    });
}

pub fn start() {
    let (wake, woken) = async_channel::unbounded::<()>();
    WAKE.with(|w| *w.borrow_mut() = Some(wake));
    glib::spawn_future_local(async move {
        loop {
            update(|u| u.refreshing = true);
            let now = glib::real_time() / 1_000_000;
            let result = fetch_limits().await;
            let stats = gio::spawn_blocking(scan_transcripts).await.unwrap_or_default();
            update(|u| {
                u.refreshing = false;
                u.checked_at = Some(now);
                u.days = stats.0;
                u.models = stats.1;
                match result {
                    Ok((session, weekly, plan)) => {
                        u.session = session.or(u.session);
                        u.weekly = weekly.or(u.weekly);
                        u.plan = plan;
                        u.updated_at = Some(now);
                        u.problem = None;
                    }
                    Err(problem) => u.problem = Some(problem),
                }
            });
            let tick = glib::timeout_future(POLL);
            let nudged = woken.recv();
            crate::bar::first_of(tick, nudged).await;
        }
    });
}

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "subscriptionType", default)]
    subscription: String,
    #[serde(rename = "rateLimitTier", default)]
    tier: String,
}

#[derive(Deserialize)]
struct Windows {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
    seven_day_oauth_apps: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| glib::home_dir().join(".claude"))
}

/// "max_20x" tiers read as "Max 20X"; otherwise the subscription, capitalised.
fn plan_label(oauth: &OAuth) -> String {
    if let Some(multiple) = oauth.tier.to_lowercase().split("max_").nth(1).and_then(|r| r.split('_').next()).filter(|m| m.ends_with('x')) {
        return format!("Max {}", multiple.to_uppercase());
    }
    let mut chars = oauth.subscription.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

async fn fetch_limits() -> Result<(Option<Limit>, Option<Limit>, String), String> {
    let raw = std::fs::read_to_string(claude_dir().join(".credentials.json")).map_err(|_| "Sign in to Claude Code to see its limits.".to_owned())?;
    let oauth = serde_json::from_str::<Credentials>(&raw).ok().and_then(|c| c.oauth).ok_or("Sign in to Claude Code to see its limits.")?;
    // A curl config file on stdin carries the headers.
    let config = format!(
        "header = \"Authorization: Bearer {}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\n",
        oauth.access_token
    );
    let process = gio::Subprocess::newv(
        &["curl", "-s", "--max-time", "8", "-w", "\n%{http_code}", "-K", "-", ENDPOINT].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .map_err(|e| e.to_string())?;
    let (stdout, _) = process.communicate_utf8_future(Some(config)).await.map_err(|e| e.to_string())?;
    let stdout = stdout.map(String::from).unwrap_or_default();
    // curl prints an error status's body like any other, so the status
    // rides on its own last line.
    let (body, status) = stdout.rsplit_once('\n').unwrap_or(("", ""));
    match status.trim().parse::<u32>().unwrap_or(0) {
        200..=299 => {}
        429 => return Err("The usage endpoint is rate-limiting checks. Retrying shortly.".into()),
        0 => return Err("Couldn't reach the usage endpoint.".into()),
        code => return Err(format!("The usage endpoint answered with status {code}.")),
    }
    let windows: Windows = serde_json::from_str(body).map_err(|_| "The usage endpoint sent something unexpected.".to_owned())?;
    let weekly = windows.seven_day_oauth_apps.or(windows.seven_day);
    // The endpoint has reported both fractions and percentages; any value
    // of 1 or more means percentages.
    let values = [windows.five_hour.as_ref(), weekly.as_ref()].map(|w| w.and_then(|w| w.utilization));
    let percentages = values.iter().flatten().any(|v| *v >= 1.0);
    let limit = |w: Option<Window>| {
        let w = w?;
        let u = w.utilization?;
        let percent = if percentages { u } else { u * 100.0 };
        let resets_at = w.resets_at.and_then(|r| glib::DateTime::from_iso8601(&r, None).ok()).map(|d| d.to_unix());
        Some(Limit { percent: percent.round().clamp(0.0, 100.0) as u32, resets_at })
    };
    let (session, weekly) = (limit(windows.five_hour), limit(weekly));
    if session.is_none() && weekly.is_none() {
        return Err("The usage endpoint returned no limits.".into());
    }
    Ok((session, weekly, plan_label(&oauth)))
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    timestamp: String,
    #[serde(rename = "requestId", default)]
    request_id: String,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    role: Option<String>,
    model: Option<String>,
    usage: Option<Tokens>,
}

#[derive(Deserialize)]
struct Tokens {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// Tokens by local day (oldest first, the last seven) and by model
/// (largest first). Runs on a worker thread.
fn scan_transcripts() -> (Vec<(String, u64)>, Vec<(String, u64)>) {
    let cutoff = SystemTime::now() - Duration::from_secs(SCAN_DAYS * 86_400);
    let mut files = Vec::new();
    collect_transcripts(&claude_dir().join("projects"), cutoff, &mut files);

    let mut seen = HashSet::new();
    let mut days: BTreeMap<String, u64> = BTreeMap::new();
    let mut models: HashMap<String, u64> = HashMap::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for line in text.lines().filter(|l| l.contains("\"usage\"")) {
            let Ok(line) = serde_json::from_str::<Line>(line) else { continue };
            let Some(message) = line.message else { continue };
            if line.kind != "assistant" && message.role.as_deref() != Some("assistant") {
                continue;
            }
            let Some(t) = message.usage else { continue };
            let tokens = t.input_tokens + t.output_tokens + t.cache_read_input_tokens + t.cache_creation_input_tokens;
            // A message is written once per content block; count it once.
            let id = message.id.unwrap_or(line.request_id);
            if tokens == 0 || (!id.is_empty() && !seen.insert(id)) {
                continue;
            }
            let Some(day) = local_day(&line.timestamp) else { continue };
            *days.entry(day).or_default() += tokens;
            *models.entry(message.model.unwrap_or_else(|| "claude".into())).or_default() += tokens;
        }
    }
    let days: Vec<_> = days.into_iter().collect();
    let days = days[days.len().saturating_sub(7)..].to_vec();
    let mut models: Vec<_> = models.into_iter().filter(|(m, _)| m != "<synthetic>").collect();
    models.sort_by(|a, b| b.1.cmp(&a.1));
    (days, models)
}

fn collect_transcripts(dir: &Path, cutoff: SystemTime, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect_transcripts(&path, cutoff, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") && meta.modified().is_ok_and(|m| m >= cutoff) {
            out.push(path);
        }
    }
}

/// The local calendar day of an ISO 8601 timestamp.
fn local_day(timestamp: &str) -> Option<String> {
    let utc = glib::DateTime::from_iso8601(timestamp, None).ok()?;
    Some(utc.to_local().ok()?.format("%Y-%m-%d").ok()?.to_string())
}

/// "1.2M", "34.5K", "812".
pub fn format_tokens(n: u64) -> String {
    let n = n as f64;
    match n {
        n if n >= 1e9 => format!("{:.1}B", n / 1e9),
        n if n >= 1e6 => format!("{:.1}M", n / 1e6),
        n if n >= 1e3 => format!("{:.1}K", n / 1e3),
        n => format!("{n}"),
    }
}
