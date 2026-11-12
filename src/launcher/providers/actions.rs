//! Actions: "/" lists commands (open a shell panel, lock, change the
//! wallpaper, take a screenshot, media keys...), grouped into tabs while the
//! query is a bare "/", and fuzzy-matched by name and keywords after it.
//! Log out, restart and shut down go through the power menu, which asks for
//! a held press, rather than running from a stray Enter.

use std::rc::Rc;

use crate::launcher::results::{Entry, Icon};

pub const TABS: &[&str] = &["All", "Shell", "Session", "Look", "Tools", "Media", "Network"];

enum Run {
    /// `eclipse-shell <args>`, which reaches the running shell.
    Shell(&'static [&'static str]),
    /// A shell command line, run detached.
    Sh(&'static str),
}

struct Action {
    name: &'static str,
    /// Extra words it's found by.
    keywords: &'static str,
    glyph: &'static str,
    tab: &'static str,
    run: Run,
}

const CATALOG: &[Action] = &[
    Action { name: "Overview", keywords: "workspaces windows expose", glyph: "grid_view", tab: "Shell", run: Run::Shell(&["toggle-overview"]) },
    Action { name: "Clipboard History", keywords: "paste copy cliphist", glyph: "content_paste", tab: "Shell", run: Run::Shell(&["toggle-clipboard"]) },
    Action { name: "Controls", keywords: "sidebar quick settings toggles", glyph: "tune", tab: "Shell", run: Run::Shell(&["toggle-sidebar-right"]) },
    Action { name: "System Profile", keywords: "sidebar about info specs", glyph: "badge", tab: "Shell", run: Run::Shell(&["toggle-sidebar-left"]) },
    Action { name: "Calendar", keywords: "date month", glyph: "calendar_month", tab: "Shell", run: Run::Shell(&["toggle-calendar"]) },
    Action { name: "Weather", keywords: "forecast temperature", glyph: "partly_cloudy_day", tab: "Shell", run: Run::Shell(&["toggle-weather"]) },
    Action { name: "Resources", keywords: "cpu memory processes task manager", glyph: "monitoring", tab: "Tools", run: Run::Shell(&["toggle-resources"]) },
    Action { name: "Battery", keywords: "power charge", glyph: "battery_full", tab: "Tools", run: Run::Shell(&["toggle-battery"]) },
    Action { name: "Lock Screen", keywords: "lock away", glyph: "lock", tab: "Session", run: Run::Sh(crate::power::LOCK) },
    Action { name: "Sleep", keywords: "suspend", glyph: "bedtime", tab: "Session", run: Run::Sh("acpiconf -s 3") },
    Action { name: "Power Menu", keywords: "log out logout restart reboot shut down shutdown power off", glyph: "power_settings_new", tab: "Session", run: Run::Shell(&["toggle-power"]) },
    Action { name: "Next Wallpaper", keywords: "background shuffle", glyph: "shuffle", tab: "Look", run: Run::Shell(&["wallpaper-next"]) },
    Action { name: "Wallpaper Switcher", keywords: "background pick", glyph: "wallpaper", tab: "Look", run: Run::Shell(&["toggle-wallpapers"]) },
    Action { name: "Walls", keywords: "wallpaper browser download wallhaven live", glyph: "imagesmode", tab: "Look", run: Run::Sh("eclipse-shell walls") },
    Action { name: "Night Light", keywords: "warm blue light hyprsunset", glyph: "nightlight", tab: "Look", run: Run::Sh("pkill -x hyprsunset || hyprsunset -t 4500") },
    Action { name: "Display", keywords: "brightness vibrance", glyph: "light_mode", tab: "Look", run: Run::Shell(&["toggle-display"]) },
    Action { name: "Screenshot Area", keywords: "capture grab snip region", glyph: "screenshot_region", tab: "Tools", run: Run::Sh("sleep 0.3; d=\"$HOME/Pictures/Screenshots\"; mkdir -p \"$d\"; f=\"$d/$(date +%Y-%m-%d_%H-%M-%S).png\"; g=\"$(slurp)\" && grim -g \"$g\" \"$f\" && wl-copy < \"$f\" && notify-send -i \"$f\" Screenshot \"Saved and copied\"") },
    Action { name: "Screenshot Screen", keywords: "capture grab full", glyph: "screenshot_monitor", tab: "Tools", run: Run::Sh("sleep 0.4; d=\"$HOME/Pictures/Screenshots\"; mkdir -p \"$d\"; f=\"$d/$(date +%Y-%m-%d_%H-%M-%S).png\"; grim \"$f\" && wl-copy < \"$f\" && notify-send -i \"$f\" Screenshot \"Saved and copied\"") },
    Action { name: "Colour Picker", keywords: "color eyedropper hex pick", glyph: "colorize", tab: "Tools", run: Run::Sh("sleep 0.3; c=\"$(hyprpicker -a)\" && notify-send \"Colour copied\" \"$c\"") },
    Action { name: "Play / Pause", keywords: "music media toggle", glyph: "play_pause", tab: "Media", run: Run::Sh("playerctl play-pause") },
    Action { name: "Next Track", keywords: "music media skip", glyph: "skip_next", tab: "Media", run: Run::Sh("playerctl next") },
    Action { name: "Previous Track", keywords: "music media back", glyph: "skip_previous", tab: "Media", run: Run::Sh("playerctl previous") },
    Action { name: "Now Playing", keywords: "music media player", glyph: "music_note", tab: "Media", run: Run::Shell(&["toggle-media"]) },
    Action { name: "Volume Mixer", keywords: "sound audio output input", glyph: "volume_up", tab: "Media", run: Run::Shell(&["toggle-mixer"]) },
    Action { name: "Wi-Fi", keywords: "wifi wireless network internet", glyph: "wifi", tab: "Network", run: Run::Shell(&["toggle-network"]) },
    Action { name: "Bluetooth", keywords: "headphones pair devices", glyph: "bluetooth", tab: "Network", run: Run::Shell(&["toggle-bluetooth"]) },
    Action { name: "Network Traffic", keywords: "speed download upload bandwidth", glyph: "swap_vert", tab: "Network", run: Run::Shell(&["toggle-traffic"]) },
];

fn shell(args: &[&str]) {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "eclipse-shell".into());
    let mut argv = vec![exe.as_str()];
    argv.extend_from_slice(args);
    crate::audio::spawn(&argv);
}

fn entry(action: &'static Action, matched: Vec<(usize, usize)>) -> Entry {
    Entry {
        icon: Icon::Glyph(action.glyph),
        title: action.name.to_owned(),
        subtitle: action.tab.to_owned(),
        matched,
        verb: "Run",
        action: Rc::new(move || match action.run {
            Run::Shell(args) => shell(args),
            Run::Sh(line) => crate::audio::spawn(&["sh", "-c", line]),
        }),
    }
}

/// Actions matching `query`; with an empty query, the chosen tab's.
pub fn search(query: &str, tab: usize) -> Vec<Entry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        let tab = TABS.get(tab).copied().unwrap_or("All");
        return CATALOG.iter().filter(|a| tab == "All" || a.tab == tab).map(|a| entry(a, Vec::new())).collect();
    }
    let mut scored: Vec<(u32, &'static Action, Vec<(usize, usize)>)> = CATALOG
        .iter()
        .filter_map(|a| {
            let name = a.name.to_lowercase();
            if let Some(at) = name.find(&query) {
                // ASCII names, so lowercasing keeps byte offsets.
                return Some((if at == 0 { 3 } else { 2 }, a, vec![(at, at + query.len())]));
            }
            if a.keywords.split(' ').any(|k| k.starts_with(&query)) {
                return Some((1, a, Vec::new()));
            }
            None
        })
        .collect();
    scored.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, a, m)| entry(a, m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_name_and_keyword() {
        assert_eq!(search("lock", 0).first().map(|e| e.title.clone()), Some("Lock Screen".into()));
        assert_eq!(search("reboot", 0).first().map(|e| e.title.clone()), Some("Power Menu".into()));
        assert!(search("", 2).iter().all(|e| e.subtitle == "Session"));
        assert_eq!(search("", 0).len(), CATALOG.len());
    }

    #[test]
    fn tabs_are_known() {
        assert!(CATALOG.iter().all(|a| TABS.contains(&a.tab)));
    }
}
