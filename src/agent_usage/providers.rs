use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use gio::prelude::*;
use gtk4::{gio, glib};
use serde_json::{Value, json};

use super::{Limit, Provider, Quota, SCAN_DAYS, TokenStats, collect_transcripts, local_day};

fn cli(args: &[&str], flags: gio::SubprocessFlags) -> Result<gio::Subprocess, String> {
    let argv: Vec<_> = args.iter().map(OsStr::new).collect();
    gio::Subprocess::newv(
        &argv,
        flags | gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .map_err(|_| format!("Install {} to see usage.", args[0]))
}

pub(super) async fn codex_limits() -> Result<Quota, String> {
    let process = cli(
        &["codex", "-s", "read-only", "-a", "never", "app-server"],
        gio::SubprocessFlags::STDIN_PIPE,
    )?;
    let timeout = glib::timeout_add_local_once(std::time::Duration::from_secs(12), {
        let process = process.clone();
        move || process.force_exit()
    });
    // Remove the timer only if it hasn't already fired.
    let result = codex_rpc(&process).await;
    if let Some(source) = glib::MainContext::default().find_source_by_id(&timeout) {
        source.destroy();
    }
    process.force_exit();
    result
}

async fn codex_rpc(process: &gio::Subprocess) -> Result<Quota, String> {
    let input = process.stdin_pipe().unwrap();
    let output = gio::DataInputStream::new(&process.stdout_pipe().unwrap());
    let send = |message: Value| input.write_all_future(format!("{message}\n"), glib::Priority::DEFAULT);
    let help = "Run `codex login` to see usage.";
    let mut account = Value::Null;
    for (id, method, params) in [
        (
            1,
            "initialize",
            json!({"clientInfo": {"name": "eclipse-shell", "version": env!("CARGO_PKG_VERSION")}}),
        ),
        (2, "account/read", json!({})),
        (3, "account/rateLimits/read", json!({})),
    ] {
        let (_, _, error) = send(json!({"id": id, "method": method, "params": params}))
            .await
            .map_err(|_| help)?;
        if error.is_some() {
            return Err(help.into());
        }
        let reply = loop {
            let line = output
                .read_line_utf8_future(glib::Priority::DEFAULT)
                .await
                .map_err(|_| help)?
                .ok_or(help)?;
            if let Ok(reply) = serde_json::from_str::<Value>(&line) {
                if reply["id"].as_i64() == Some(id) {
                    break reply;
                }
            }
        };
        match id {
            1 => {
                if reply.get("error").is_some() {
                    return Err(help.into());
                }
                let (_, _, error) = send(json!({"method": "initialized", "params": {}})).await.map_err(|_| help)?;
                if error.is_some() {
                    return Err(help.into());
                }
            }
            2 => account = reply["result"]["account"].clone(),
            _ => {
                let limits = &reply["result"]["rateLimits"];
                let window = |window: &Value| {
                    let percent = window["usedPercent"].as_f64()?.round().clamp(0.0, 100.0) as u32;
                    let resets_at = window["resetsAt"]
                        .as_i64()
                        .filter(|&t| t > 0)
                        .map(|t| if t >= 1_000_000_000_000 { t / 1000 } else { t });
                    Some(Limit { percent, resets_at })
                };
                let limits_pair = (window(&limits["primary"]), window(&limits["secondary"]));
                if limits_pair.0.is_none() && limits_pair.1.is_none() {
                    return Err("Codex returned no usage limits. Run `codex login` to check your account.".into());
                }
                let plan = [limits["planType"].as_str(), account["planType"].as_str(), account["type"].as_str()]
                    .into_iter()
                    .flatten()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                return Ok(Quota {
                    limits: limits_pair,
                    plan,
                    ..Quota::default()
                });
            }
        }
    }
    unreachable!()
}

fn antigravity_dir() -> PathBuf {
    std::env::var_os("GEMINI_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::home_dir().join(".gemini/antigravity-cli"))
}

fn antigravity_plan() -> String {
    let read = |file| -> Value {
        std::fs::read(antigravity_dir().join(file))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    };
    let settings = read("settings.json");
    std::env::var("GEMINI_PLAN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("GOOGLE_AI_PLAN").ok().filter(|s| !s.is_empty()))
        .or_else(|| {
            settings["plan"]
                .as_str()
                .or_else(|| settings["tier"].as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            if read("cache/onboarding.json")["enterpriseOnboardingComplete"].as_bool() == Some(true) {
                "Enterprise".into()
            } else {
                "Pro".into()
            }
        })
}

pub(super) async fn antigravity_limits() -> Result<Quota, String> {
    let process = cli(&["agy", "--print", "/usage"], gio::SubprocessFlags::NONE)?;
    let timeout = glib::timeout_add_local_once(std::time::Duration::from_secs(12), {
        let process = process.clone();
        move || process.force_exit()
    });
    let result = process.communicate_utf8_future(None).await;
    if let Some(source) = glib::MainContext::default().find_source_by_id(&timeout) {
        source.destroy();
    }
    let (output, _) = result.map_err(|_| "Couldn't read Antigravity usage.")?;
    parse_antigravity(output.as_deref().unwrap_or_default())
        .map(|mut quota| {
            quota.plan = antigravity_plan();
            quota
        })
        .ok_or_else(|| "Run `agy` to see usage.".into())
}

fn parse_antigravity(output: &str) -> Option<Quota> {
    let mut quota = Quota::default();
    let mut found = false;
    for line in output.lines() {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let (pool, row) = if let Some(row) = normalized.split_once("Gemini Models ") {
            (&mut quota.limits, row.1)
        } else if let Some(row) = normalized.split_once("Claude and GPT models ") {
            (&mut quota.partner, row.1)
        } else {
            continue;
        };
        let (window, rest) = if let Some(rest) = row.strip_prefix("Five Hour Limit Remaining ") {
            (&mut pool.0, rest)
        } else if let Some(rest) = row.strip_prefix("Weekly Limit Remaining ") {
            (&mut pool.1, rest)
        } else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let Some(remaining) = fields.next().and_then(|s| s.strip_suffix('%')).and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let resets_at = fields
            .next()
            .and_then(|s| glib::DateTime::from_iso8601(s, None).ok())
            .map(|d| d.to_unix());
        *window = Some(Limit {
            percent: 100_u32.saturating_sub(remaining),
            resets_at,
        });
        found = true;
    }
    found.then_some(quota)
}

pub(super) fn scan_transcripts(provider: Provider) -> TokenStats {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    let cutoff = SystemTime::now() - Duration::from_secs(SCAN_DAYS * 86_400);
    let mut files = Vec::new();
    let codex = provider == Provider::Codex;
    if codex {
        let dir = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| glib::home_dir().join(".codex"));
        for subdir in ["sessions", "archived_sessions"] {
            collect_transcripts(&dir.join(subdir), cutoff, &mut files);
        }
    } else {
        collect_transcripts(&antigravity_dir().join("brain"), cutoff, &mut files);
        files.retain(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("transcript"))
        });
    }

    let dates: Vec<_> = glib::DateTime::now_local()
        .into_iter()
        .flat_map(|now| {
            (0..7)
                .rev()
                .filter_map(move |days| now.add_days(-days).ok()?.format("%Y-%m-%d").ok().map(String::from))
        })
        .collect();
    let mut days: Vec<_> = dates.into_iter().map(|day| (day, 0_u64)).collect();
    let mut models = HashMap::new();
    for path in files {
        let Ok(file) = std::fs::File::open(path) else { continue };
        let mut model = if codex { "codex".to_owned() } else { "Gemini 3.7 Flash".into() };
        let mut previous_total = None;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (timestamp, tokens) = if codex {
                let payload = entry.get("payload").unwrap_or(&entry);
                if entry["type"] == "turn_context" {
                    if let Some(name) = payload["model"].as_str().or_else(|| payload["model_slug"].as_str()) {
                        model = name.to_owned();
                    }
                    continue;
                }
                if payload["type"] != "token_count" {
                    continue;
                }
                let info = &payload["info"];
                // Codex can repeat the same cumulative usage on successive events.
                if let Some(total) = info["total_token_usage"]["total_tokens"].as_u64() {
                    if previous_total.replace(total) == Some(total) {
                        continue;
                    }
                }
                let usage = &info["last_token_usage"];
                let count = |field: &str| usage[field].as_u64().unwrap_or_default();
                let cached = count("cached_input_tokens").saturating_add(count("cache_write_input_tokens"));
                (
                    entry["timestamp"].as_str(),
                    count("input_tokens").max(cached).saturating_add(count("output_tokens")),
                )
            } else {
                let kind = entry["type"].as_str().unwrap_or_default();
                if !matches!(kind, "PLANNER_RESPONSE" | "USER_INPUT" | "GENERIC") {
                    continue;
                }
                let len = |field: &str| entry[field].as_str().map_or(0, |s| s.chars().count());
                let estimate = ((len("content") + len("thinking")) as f64 / 3.8) as u64;
                (
                    entry["created_at"].as_str(),
                    estimate + if kind == "PLANNER_RESPONSE" { 500 } else { 0 },
                )
            };
            if tokens == 0 {
                continue;
            }
            let Some(day) = timestamp.and_then(local_day) else { continue };
            if let Some((_, daily)) = days.iter_mut().find(|(date, _)| *date == day) {
                *daily = daily.saturating_add(tokens);
                let total = models.entry(model.clone()).or_insert(0_u64);
                *total = total.saturating_add(tokens);
            }
        }
    }
    let mut models: Vec<_> = models.into_iter().collect();
    models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    models.truncate(4);
    (days, models)
}
