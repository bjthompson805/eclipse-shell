//! The keybind legend, read from the Hyprland Lua config: every `hl.bind`
//! in `hyprland.lua` (a stock config keeps its binds there),
//! `modules/binds.lua`, the `eclipse_shell.lua` install.sh writes, then
//! `user.lua`. A comment after a blank line
//! heads a group; a bind's trailing comment says what it does, and without
//! one its command stands in.

use std::path::Path;

pub struct Group {
    pub title: String,
    pub binds: Vec<Bind>,
}

pub struct Bind {
    /// The chord, one key per cap: ["Super", "Shift", "W"].
    pub keys: Vec<String>,
    pub what: String,
    /// True when `what` is the command rather than a description.
    pub raw: bool,
}

pub fn load() -> Vec<Group> {
    let hypr = gtk4::glib::user_config_dir().join("hypr");
    let mut groups = read(&hypr.join("hyprland.lua"), "Hyprland");
    groups.extend(read(&hypr.join("modules").join("binds.lua"), "Keybinds"));
    groups.extend(read(&hypr.join("eclipse_shell.lua"), "Eclipse Shell"));
    groups.extend(read(&hypr.join("user.lua"), "Your own"));
    groups.retain(|g| !g.binds.is_empty());
    groups
}

fn read(path: &Path, first: &str) -> Vec<Group> {
    std::fs::read_to_string(path).map(|text| parse(&text, first)).unwrap_or_default()
}

fn parse(text: &str, first: &str) -> Vec<Group> {
    let mut groups = vec![Group { title: first.to_owned(), binds: Vec::new() }];
    // Lua locals holding key names, like `local mod = "SUPER"`.
    let mut locals: Vec<(String, String)> = Vec::new();
    let mut after_blank = true;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            after_blank = true;
            continue;
        }
        let blank = std::mem::replace(&mut after_blank, false);
        if let Some(comment) = line.strip_prefix("--") {
            let comment = comment.trim();
            if blank && !comment.is_empty() && !comment.starts_with("hl.") {
                groups.push(Group { title: heading(comment), binds: Vec::new() });
            }
            continue;
        }
        // A bind kept in a local, as the stock config's
        // `local closeWindowBind = hl.bind(...)`, is still a bind.
        let line = match line.strip_prefix("local ").and_then(|rest| rest.split_once('=')) {
            Some((_, value)) if value.trim_start().starts_with("hl.bind(") => value.trim_start(),
            Some((name, value)) => {
                if let Some(value) = string_literal(value.trim()) {
                    locals.push((name.trim().to_owned(), value));
                }
                continue;
            }
            None => line,
        };
        let Some(args) = line.strip_prefix("hl.bind(") else { continue };
        let (code, comment) = split_comment(args);
        let Some((key, action)) = split_top(code) else { continue };
        let keys = chord(key, &locals);
        if keys.is_empty() {
            continue;
        }
        let (what, raw) = match comment {
            Some(c) => (sentence(c), false),
            None => (command(action, &locals), true),
        };
        groups.last_mut().unwrap().binds.push(Bind { keys, what, raw });
    }
    groups
}

/// A group title from its comment: the first sentence, without the stop or
/// a trailing pointer like the stock config's ", see https://...".
fn heading(comment: &str) -> String {
    let first = comment.split(". ").next().unwrap_or(comment);
    let first = first.split(", see ").next().unwrap_or(first);
    first.trim_end_matches('.').to_owned()
}

fn sentence(comment: &str) -> String {
    let mut chars = comment.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// What a bind without a comment runs: the command of an `exec_cmd`, or the dispatcher.
fn command(action: &str, locals: &[(String, String)]) -> String {
    if let Some(at) = action.find("exec_cmd(") {
        // The argument up to its closing paren: literals and locals joined
        // by `..`, as in `shell .. " hub"`.
        let arg = call_arg(&action[at + "exec_cmd(".len()..]);
        let mut cmd = String::new();
        for part in arg.split("..").map(str::trim) {
            match string_literal(part).or_else(|| locals.iter().rev().find(|(n, _)| n == part).map(|(_, v)| v.clone())) {
                Some(s) => cmd.push_str(&s),
                None => {
                    cmd.clear();
                    break;
                }
            }
        }
        // A program by full path reads better by name.
        if cmd.starts_with('/') {
            let (program, rest) = cmd.split_once(' ').unwrap_or((&cmd, ""));
            let name = program.rsplit('/').next().unwrap_or(program);
            cmd = format!("{name} {rest}").trim_end().to_owned();
        }
        if !cmd.is_empty() {
            return cmd;
        }
    }
    let action = action.trim().trim_start_matches("hl.dsp.");
    action.split('(').next().unwrap_or(action).replace(['.', '_'], " ")
}

/// Splits a line's code from its trailing `--` comment, skipping any inside strings.
fn split_comment(line: &str) -> (&str, Option<&str>) {
    let mut quote = None;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match (quote, bytes[i]) {
            (Some(_), b'\\') => i += 1,
            (Some(q), c) if c == q => quote = None,
            (None, b'"' | b'\'') => quote = Some(bytes[i]),
            (None, b'-') if bytes.get(i + 1) == Some(&b'-') => return (&line[..i], Some(line[i + 2..].trim())),
            _ => {}
        }
        i += 1;
    }
    (line, None)
}

/// A call's argument text, `text` starting just inside its paren: up to the
/// paren that closes it, skipping any inside strings.
fn call_arg(text: &str) -> &str {
    let mut depth = 0i32;
    let mut quote = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '{' | '[') => depth += 1,
            (None, ')') if depth == 0 => return &text[..i],
            (None, ')' | '}' | ']') => depth -= 1,
            _ => {}
        }
    }
    text
}

/// Splits `hl.bind`'s arguments at the first top-level comma: the key, then the rest.
fn split_top(args: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut quote = None;
    for (i, c) in args.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '{' | '[') => depth += 1,
            (None, ')' | '}' | ']') => depth -= 1,
            (None, ',') if depth == 0 => return Some((&args[..i], &args[i + 1..])),
            _ => {}
        }
    }
    None
}

/// The leading string literal of `text`, unquoted.
pub(super) fn string_literal(text: &str) -> Option<String> {
    let text = text.trim_start();
    let q = text.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let body = &text[1..];
    let end = body.find(q)?;
    Some(body[..end].replace("\\\"", "\""))
}

/// The key expression, e.g. `mod .. " + SHIFT + " .. key`, as caps.
fn chord(expr: &str, locals: &[(String, String)]) -> Vec<String> {
    let mut spelled = String::new();
    for part in expr.split("..").map(str::trim) {
        if let Some(s) = string_literal(part) {
            spelled.push_str(&s);
        } else if let Some((_, v)) = locals.iter().rev().find(|(n, _)| n == part) {
            spelled.push_str(v);
        } else {
            // A loop variable, as in the workspace binds.
            spelled.push_str("1–0");
        }
    }
    spelled.split('+').map(str::trim).filter(|k| !k.is_empty()).map(key_name).collect()
}

fn key_name(key: &str) -> String {
    let named = match key.to_ascii_uppercase().as_str() {
        "SUPER" | "MOD4" => "Super",
        "SHIFT" => "Shift",
        "CTRL" | "CONTROL" => "Ctrl",
        "ALT" | "MOD1" => "Alt",
        "RETURN" => "Enter",
        "ESCAPE" => "Esc",
        "SPACE" => "Space",
        "TAB" => "Tab",
        "COMMA" => ",",
        "PERIOD" => ".",
        "MOUSE:272" => "Left drag",
        "MOUSE:273" => "Right drag",
        "MOUSE_UP" => "Scroll up",
        "MOUSE_DOWN" => "Scroll down",
        "LEFT" => "←",
        "RIGHT" => "→",
        "UP" => "↑",
        "DOWN" => "↓",
        "XF86AUDIORAISEVOLUME" => "Volume up",
        "XF86AUDIOLOWERVOLUME" => "Volume down",
        "XF86AUDIOMUTE" => "Mute",
        "XF86AUDIOPLAY" => "Play",
        "XF86AUDIONEXT" => "Next track",
        "XF86AUDIOPREV" => "Previous track",
        "XF86MONBRIGHTNESSUP" => "Brightness up",
        "XF86MONBRIGHTNESSDOWN" => "Brightness down",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    let key = key.strip_prefix("XF86").unwrap_or(key);
    if key.chars().count() == 1 { key.to_uppercase() } else { key.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"local mod = "SUPER"

-- Windows
hl.bind(mod .. " + Q",         hl.dsp.window.close())                           -- close active window
hl.bind(mod .. " + Return",    hl.dsp.exec_cmd("kitty"))
hl.bind(mod .. " + mouse:272", hl.dsp.window.drag(),   { mouse = true })

-- Workspaces. Super+N focuses the Nth workspace.
-- More words.
for i = 1, 10 do
    local key = i % 10 -- 10 maps to the 0 key
    hl.bind(mod .. " + ALT + " .. key,    hl.dsp.exec_cmd(ws_helper .. " move " .. i))  -- send active window to slot i
end
hl.bind("XF86AudioMute", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle; eclipse-shell osd volume"), { locked = true })
-- hl.bind("SUPER + SHIFT + T", hl.dsp.exec_cmd("kitty"))
"#;

    /// The stock hyprland.lua and the eclipse_shell.lua install.sh writes.
    const STOCK: &str = r#"local terminal = "kitty"
local mainMod = "SUPER"
local shell = "/home/me/.local/bin/eclipse-shell"

-- Example binds, see https://wiki.hypr.land/Configuring/Basics/Binds/ for more
hl.bind(mainMod .. " + Q", hl.dsp.exec_cmd(terminal))
local closeWindowBind = hl.bind(mainMod .. " + C", hl.dsp.window.close())
-- closeWindowBind:set_enabled(false)
hl.bind("SUPER + ALT + W",    hl.dsp.exec_cmd(shell .. " wallpaper-next"))
hl.bind("XF86AudioMute", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"), { locked = true, repeating = true })
"#;

    #[test]
    fn stock_config() {
        let groups = parse(STOCK, "Hyprland");
        let binds: Vec<&Bind> = groups.iter().flat_map(|g| &g.binds).collect();
        assert_eq!(groups.last().unwrap().title, "Example binds");
        assert_eq!(binds.len(), 4);
        assert_eq!((binds[0].keys.join("+"), binds[0].what.as_str()), ("Super+Q".to_owned(), "kitty"));
        assert_eq!((binds[1].keys.join("+"), binds[1].what.as_str()), ("Super+C".to_owned(), "window close"));
        assert_eq!((binds[2].keys.join("+"), binds[2].what.as_str()), ("Super+Alt+W".to_owned(), "eclipse-shell wallpaper-next"));
        assert_eq!(binds[3].what, "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle");
    }

    #[test]
    fn groups_and_binds() {
        let groups = parse(SAMPLE, "Keybinds");
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Keybinds", "Windows", "Workspaces"]);
        let windows = &groups[1].binds;
        assert_eq!(windows[0].keys, ["Super", "Q"]);
        assert_eq!(windows[0].what, "Close active window");
        assert!(!windows[0].raw);
        assert_eq!(windows[1].keys, ["Super", "Enter"]);
        assert_eq!((windows[1].what.as_str(), windows[1].raw), ("kitty", true));
        assert_eq!(windows[2].keys, ["Super", "Left drag"]);
        assert_eq!(windows[2].what, "window drag");
        let ws = &groups[2].binds;
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].keys, ["Super", "Alt", "1–0"]);
        assert_eq!(ws[1].keys, ["Mute"]);
        assert!(ws[1].what.starts_with("wpctl set-mute"));
    }
}
