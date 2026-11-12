//! Media players over MPRIS: follows every `org.mpris.MediaPlayer2.*` name on
//! the session bus and picks the one to show, the first playing player or
//! else the first one present. A live wallpaper (mpvpaper) registers too;
//! a bare video file name for a title marks it as scenery, not music.

use std::cell::RefCell;
use std::time::Duration;

use gtk::glib::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";

#[derive(Clone, Default, PartialEq)]
pub struct Player {
    pub name: String,
    pub playing: bool,
    pub title: String,
    pub artists: String,
    pub album: String,
    pub art_url: String,
    /// The player's own name for itself, e.g. "Firefox".
    pub identity: String,
    /// The process behind the bus name, to find its audio stream by.
    pub pid: Option<u32>,
    /// The track's length, None for a live stream.
    pub length: Option<Duration>,
    pub track_id: String,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_seek: bool,
    /// None when the player doesn't offer it.
    pub shuffle: Option<bool>,
    pub repeat: Option<Repeat>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Repeat {
    None,
    Track,
    Playlist,
}

impl Repeat {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "None" => Some(Repeat::None),
            "Track" => Some(Repeat::Track),
            "Playlist" => Some(Repeat::Playlist),
            _ => None,
        }
    }

    /// What a click on the repeat button moves to: off, all, one, off.
    pub fn next(self) -> Self {
        match self {
            Repeat::None => Repeat::Playlist,
            Repeat::Playlist => Repeat::Track,
            Repeat::Track => Repeat::None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Repeat::None => "None",
            Repeat::Track => "Track",
            Repeat::Playlist => "Playlist",
        }
    }
}

impl Player {
    /// "Title · Artist", or just the title.
    pub fn line(&self) -> String {
        if self.artists.is_empty() { self.title.clone() } else { format!("{} \u{b7} {}", self.title, self.artists) }
    }

    fn is_wallpaper(&self) -> bool {
        let t = self.title.to_lowercase();
        [".mp4", ".webm", ".mkv", ".gif"].iter().any(|ext| t.ends_with(ext))
    }
}

thread_local! {
    static CONNECTION: RefCell<Option<gio::DBusConnection>> = RefCell::default();
    static PLAYERS: RefCell<Vec<Player>> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
    static SUBSCRIPTIONS: RefCell<Vec<(String, gio::SignalSubscription)>> = RefCell::default();
    /// The length each track last reported, and the tracks found to be live:
    /// a stream (mpv playing a radio, say) can report what it has buffered as
    /// its length, which grows as it plays.
    static LENGTHS: RefCell<std::collections::HashMap<String, i64>> = RefCell::default();
    static LIVE: RefCell<std::collections::HashSet<String>> = RefCell::default();
}

/// Every player with a track, live wallpapers left out.
pub fn players() -> Vec<Player> {
    PLAYERS.with(|p| p.borrow().iter().filter(|p| !p.is_wallpaper() && !p.title.is_empty()).cloned().collect())
}

/// The player to show, if any has a track.
pub fn current() -> Option<Player> {
    PLAYERS.with(|p| {
        let p = p.borrow();
        let real: Vec<&Player> = p.iter().filter(|p| !p.is_wallpaper()).collect();
        real.iter().find(|p| p.playing).or(real.first()).map(|p| (*p).clone())
    })
    .filter(|p| !p.title.is_empty())
}

pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

fn changed() {
    LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
}

pub fn start() {
    glib::spawn_future_local(async {
        let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await else { return };
        CONNECTION.with(|c| *c.borrow_mut() = Some(connection.clone()));

        // Players coming and going.
        let owners = connection.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            None,
            gio::DBusSignalFlags::NONE,
            |signal| {
                let Some((name, _, new_owner)) = signal.parameters.get::<(String, String, String)>() else { return };
                if !name.starts_with(PREFIX) {
                    return;
                }
                if new_owner.is_empty() { remove(&name) } else { add(&name) }
            },
        );
        std::mem::forget(owners);

        let reply = connection
            .call_future(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ListNames",
                None,
                Some(glib::VariantTy::new("(as)").unwrap()),
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        if let Some((names,)) = reply.ok().and_then(|r| r.get::<(Vec<String>,)>()) {
            names.iter().filter(|n| n.starts_with(PREFIX)).for_each(|n| add(n));
        }
    });
}

fn connection() -> Option<gio::DBusConnection> {
    CONNECTION.with(|c| c.borrow().clone())
}

fn add(name: &str) {
    let Some(connection) = connection() else { return };
    if PLAYERS.with(|p| p.borrow().iter().any(|p| p.name == name)) {
        return;
    }
    PLAYERS.with(|p| p.borrow_mut().push(Player { name: name.to_owned(), ..Player::default() }));
    let subscription = connection.subscribe_to_signal(
        Some(name),
        Some("org.freedesktop.DBus.Properties"),
        Some("PropertiesChanged"),
        Some(PATH),
        None,
        gio::DBusSignalFlags::NONE,
        {
            let name = name.to_owned();
            move |_| refresh(&name)
        },
    );
    SUBSCRIPTIONS.with(|s| s.borrow_mut().push((name.to_owned(), subscription)));
    refresh(name);
    learn_identity(&connection, name);
}

/// The player's display name and process, which don't change while it runs.
fn learn_identity(connection: &gio::DBusConnection, name: &str) {
    let (connection, name) = (connection.clone(), name.to_owned());
    glib::spawn_future_local(async move {
        let call = |dest: &str, path: &str, iface: &str, method: &str, args: glib::Variant| {
            connection.call_future(Some(dest), path, iface, method, Some(&args), None, gio::DBusCallFlags::NONE, 5000)
        };
        let identity = call(&name, PATH, "org.freedesktop.DBus.Properties", "Get", (ROOT_IFACE, "Identity").to_variant())
            .await
            .ok()
            .and_then(|r| r.child_value(0).as_variant())
            .and_then(|v| v.get::<String>())
            .unwrap_or_default();
        let pid = call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", (name.as_str(),).to_variant())
            .await
            .ok()
            .and_then(|r| r.get::<(u32,)>())
            .map(|(pid,)| pid);
        let found = PLAYERS.with(|p| {
            let mut p = p.borrow_mut();
            let slot = p.iter_mut().find(|p| p.name == name)?;
            slot.identity = identity;
            slot.pid = pid;
            Some(())
        });
        if found.is_some() {
            changed();
        }
    });
}

fn remove(name: &str) {
    PLAYERS.with(|p| p.borrow_mut().retain(|p| p.name != name));
    SUBSCRIPTIONS.with(|s| s.borrow_mut().retain(|(n, _)| n != name));
    changed();
}

fn refresh(name: &str) {
    let Some(connection) = connection() else { return };
    let name = name.to_owned();
    glib::spawn_future_local(async move {
        let reply = connection
            .call_future(
                Some(&name),
                PATH,
                "org.freedesktop.DBus.Properties",
                "GetAll",
                Some(&(PLAYER_IFACE,).to_variant()),
                Some(glib::VariantTy::new("(a{sv})").unwrap()),
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        let Ok(reply) = reply else { return };
        let props = glib::VariantDict::new(Some(&reply.child_value(0)));
        let status = props.lookup::<String>("PlaybackStatus").ok().flatten().unwrap_or_default();
        let meta = props.lookup_value("Metadata", None).map(|m| glib::VariantDict::new(Some(&m)));
        let text = |k: &str| meta.as_ref().and_then(|m| m.lookup::<String>(k).ok().flatten()).unwrap_or_default();
        let artists = meta
            .as_ref()
            .and_then(|m| m.lookup::<Vec<String>>("xesam:artist").ok().flatten())
            .map(|a| a.join(", "))
            .unwrap_or_default();
        let flag = |k: &str| props.lookup::<bool>(k).ok().flatten().unwrap_or(false);
        // mpris:length is microseconds, as an x or sometimes a t.
        let length = meta.as_ref().and_then(|m| m.lookup_value("mpris:length", None)).and_then(|v| v.get::<i64>().or_else(|| v.get::<u64>().map(|u| u as i64)));
        let track_id = meta
            .as_ref()
            .and_then(|m| m.lookup_value("mpris:trackid", None))
            .and_then(|v| v.str().map(String::from))
            .unwrap_or_default();
        // A length that changes while the track stays the same is a live stream's.
        let track_key = format!("{name}\u{1f}{track_id}\u{1f}{}", text("xesam:title"));
        let live = match length {
            Some(raw) => {
                let before = LENGTHS.with(|l| l.borrow_mut().insert(track_key.clone(), raw));
                if before.is_some_and(|b| b != raw) {
                    LIVE.with(|l| l.borrow_mut().insert(track_key.clone()));
                }
                LIVE.with(|l| l.borrow().contains(&track_key))
            }
            None => false,
        };
        let fresh = Player {
            name: name.clone(),
            playing: status == "Playing",
            title: text("xesam:title"),
            artists,
            album: text("xesam:album"),
            art_url: text("mpris:artUrl"),
            length: length.filter(|l| *l > 0 && !live).map(|l| Duration::from_micros(l as u64)),
            track_id,
            can_previous: flag("CanGoPrevious"),
            can_next: flag("CanGoNext"),
            can_seek: flag("CanSeek") && !live,
            shuffle: props.lookup::<bool>("Shuffle").ok().flatten(),
            repeat: props.lookup::<String>("LoopStatus").ok().flatten().and_then(|l| Repeat::parse(&l)),
            ..Player::default()
        };
        let differs = PLAYERS.with(|p| {
            let mut p = p.borrow_mut();
            let slot = p.iter_mut().find(|p| p.name == name)?;
            // What the root interface told us stays.
            let updated = Player { identity: slot.identity.clone(), pid: slot.pid, ..fresh };
            (*slot != updated).then(|| *slot = updated)
        })
        .is_some();
        if differs {
            changed();
        }
    });
}

fn call(player: &Player, method: &str, args: Option<glib::Variant>) {
    let Some(connection) = connection() else { return };
    connection.call(Some(&player.name), PATH, PLAYER_IFACE, method, args.as_ref(), None, gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE, |_| {});
}

fn set(player: &Player, property: &str, value: glib::Variant) {
    let Some(connection) = connection() else { return };
    let args = (PLAYER_IFACE, property, value).to_variant();
    connection.call(Some(&player.name), PATH, "org.freedesktop.DBus.Properties", "Set", Some(&args), None, gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE, |_| {});
}

pub fn play_pause(player: &Player) {
    call(player, "PlayPause", None);
}

pub fn previous(player: &Player) {
    call(player, "Previous", None);
}

pub fn next(player: &Player) {
    call(player, "Next", None);
}

/// Jumps to `at` in the current track.
pub fn set_position(player: &Player, at: Duration) {
    let Ok(path) = glib::variant::ObjectPath::try_from(player.track_id.clone()) else { return };
    call(player, "SetPosition", Some((path, at.as_micros() as i64).to_variant()));
}

pub fn set_shuffle(player: &Player, on: bool) {
    set(player, "Shuffle", on.to_variant().into());
}

pub fn set_repeat(player: &Player, repeat: Repeat) {
    set(player, "LoopStatus", repeat.as_str().to_variant().into());
}

/// Where playback is now. Players don't announce it, so it's asked for.
pub async fn position(player: &Player) -> Option<Duration> {
    let connection = connection()?;
    let reply = connection
        .call_future(Some(&player.name), PATH, "org.freedesktop.DBus.Properties", "Get", Some(&(PLAYER_IFACE, "Position").to_variant()), None, gio::DBusCallFlags::NONE, 2000)
        .await
        .ok()?;
    let micros = reply.child_value(0).as_variant()?.get::<i64>()?;
    Some(Duration::from_micros(micros.max(0) as u64))
}
