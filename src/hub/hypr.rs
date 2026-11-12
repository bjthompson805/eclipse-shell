//! What the hub changes in Hyprland: options, read live with `getoption`
//! and evaluated at once, and commands to start at login. They're kept in
//! `~/.config/eclipse-shell/hyprland.json` and `startup.json`, and
//! written out as `~/.config/hypr/eclipse.lua`, which hyprland.lua loads last.

use std::path::PathBuf;

use gtk4::glib;
use serde_json::{Map, Value};

use crate::hyprland;

fn overrides_path() -> PathBuf {
    glib::user_config_dir().join("eclipse-shell").join("hyprland.json")
}

fn startup_path() -> PathBuf {
    glib::user_config_dir().join("eclipse-shell").join("startup.json")
}

fn lua_path() -> PathBuf {
    glib::user_config_dir().join("hypr").join("eclipse.lua")
}

/// The options the hub has changed, by name (`general:gaps_in`).
pub fn overrides() -> Map<String, Value> {
    std::fs::read_to_string(overrides_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The running value of `name`: a number, a bool or a string.
pub fn live(name: &str) -> Option<Value> {
    let v = hyprland::option_json(name)?;
    if let Some(b) = v.get("bool") {
        return Some(b.clone());
    }
    for key in ["int", "float", "str"] {
        if let Some(x) = v.get(key) {
            return Some(x.clone());
        }
    }
    // Gaps come as CSS-style "top right bottom left"; the hub sets all four alike.
    let css = v.get("css")?.as_str()?;
    css.split_whitespace().next()?.parse::<i64>().ok().map(Value::from)
}

/// Sets `name` now and for later sessions.
pub fn set(name: &str, value: Value) {
    hyprland::eval(&format!("hl.config({})", table(name, &value)));
    let mut all = overrides();
    all.insert(name.to_owned(), value);
    save(&all, &startup());
}

/// Forgets every change and reloads, so the config's own values return.
pub fn reset() {
    save(&Map::new(), &startup());
    let _ = hyprland::request("reload config-only");
}

/// The commands added in the hub to start at login.
pub fn startup() -> Vec<String> {
    std::fs::read_to_string(startup_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn set_startup(commands: &[String]) {
    save(&overrides(), commands);
}

/// Runs `command` now, from Hyprland, as it would at login.
pub fn run(command: &str) {
    let _ = hyprland::request(&format!("dispatch hl.dsp.exec_cmd({command:?})"));
}

fn save(all: &Map<String, Value>, commands: &[String]) {
    let path = overrides_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(all).unwrap_or_default() + "\n");
    let _ = std::fs::write(startup_path(), serde_json::to_string_pretty(commands).unwrap_or_default() + "\n");
    let _ = std::fs::write(lua_path(), lua(all, commands));
}

fn lua(all: &Map<String, Value>, commands: &[String]) -> String {
    let mut out = String::from("-- Written by the eclipse-shell hub (Super + ,); changes here are overwritten.\n-- Loaded last by hyprland.lua, so these win over the modules and user.lua.\n");
    for (name, value) in all {
        out.push_str(&format!("hl.config({})\n", table(name, value)));
    }
    if !commands.is_empty() {
        out.push_str("\nhl.on(\"hyprland.start\", function()\n");
        for command in commands {
            out.push_str(&format!("    hl.exec_cmd({command:?})\n"));
        }
        out.push_str("end)\n");
    }
    out
}

/// What the config starts at login, outside the hub: every `hl.exec_cmd` in
/// modules/startup.lua, and those in user.lua's `hyprland.start` handler.
pub fn configured_startup() -> Vec<(String, &'static str)> {
    let hypr = glib::user_config_dir().join("hypr");
    let mut found = Vec::new();
    for (file, label, whole) in [("modules/startup.lua", "startup.lua", true), ("user.lua", "user.lua", false)] {
        let Ok(text) = std::fs::read_to_string(hypr.join(file)) else { continue };
        let mut inside = whole;
        for line in text.lines().map(str::trim) {
            if line.starts_with("hl.on(\"hyprland.start\"") {
                inside = true;
            } else if !whole && line.starts_with("end)") {
                inside = false;
            } else if inside
                && let Some(rest) = line.strip_prefix("hl.exec_cmd(")
                && let Some(command) = super::keybinds::string_literal(rest)
            {
                found.push((command, label));
            }
        }
    }
    found
}

/// `general:gaps_in` = 8 as `{ general = { gaps_in = 8 } }`.
fn table(name: &str, value: &Value) -> String {
    let literal = match value {
        Value::String(s) => format!("{s:?}"),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => "nil".to_owned(),
    };
    name.rsplit(':').fold(literal, |inner, key| {
        if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            format!("{{ {key} = {inner} }}")
        } else {
            format!("{{ [{key:?}] = {inner} }}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        assert_eq!(table("general:gaps_in", &Value::from(8)), "{ general = { gaps_in = 8 } }");
        assert_eq!(table("input:touchpad:tap_to_click", &Value::from(true)), "{ input = { touchpad = { tap_to_click = true } } }");
        assert_eq!(table("a:odd-key", &Value::from(1)), "{ a = { [\"odd-key\"] = 1 } }");
        assert_eq!(table("general:layout", &Value::from("master")), "{ general = { layout = \"master\" } }");
        assert_eq!(table("decoration:active_opacity", &Value::from(0.9)), "{ decoration = { active_opacity = 0.9 } }");
    }

    #[test]
    fn startup_block() {
        let text = lua(&Map::new(), &["nm-applet".to_owned(), "sh -c \"a \\\"b\\\"\"".to_owned()]);
        assert!(text.contains("hl.on(\"hyprland.start\", function()\n    hl.exec_cmd(\"nm-applet\")\n    hl.exec_cmd(\"sh -c \\\"a \\\\\\\"b\\\\\\\"\\\"\")\nend)\n"));
    }
}
